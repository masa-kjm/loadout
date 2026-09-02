# Testing Strategy

## Scope

This document defines how an implementation demonstrates conformance with the v0.2.0 architecture and specifications.
It does not add runtime behavior, change an error outcome, or replace a specification.

Every behavior change must identify its owning specification and add evidence at the narrowest test layer that can prove the contract.
An observable CLI or filesystem contract also requires an integration or acceptance test; a private unit test alone is not sufficient evidence.

## Test Layers

| Layer | Purpose | Typical evidence |
| --- | --- | --- |
| Pure domain tests | Prove normalization, validation, state classification, planning, and deterministic ordering without I/O. | A Desired/Known/Actual input produces the required Plan or blocking diagnostic. |
| Filesystem contract tests | Prove no-follow observation, containment, link operations, and the absence of forbidden mutations using a temporary filesystem. | The before and after entry kinds, link targets, paths, and directory contents. |
| State repository durability tests | Prove locking, atomic replacement, schema validation, operation progress, and recovery using fault injection. | The complete state before and after an interrupted commit or recovery attempt. |
| Executor integration tests | Prove the lifecycle across real temporary files, the state repository, and the filesystem implementation. | The planned action, resulting target, post-condition, Known state, and failure aftermath. |
| CLI acceptance tests | Prove arguments, confirmation, dry-run behavior, output categories, and exit-status classes. | Process status, stdout and stderr category, and isolated filesystem and state snapshots. |
| Platform conformance tests | Prove Unix and Windows behavior that cannot be established by a platform-neutral fake. | Actual symbolic-link, reparse-point, locking, and replacement behavior on the target platform. |

## Contract Matrix

The following matrix is the minimum evidence required before v0.2.0 is considered complete.

| Contract owner | Required evidence |
| --- | --- |
| [Configuration](../specs/configuration.md) | Runtime and CLI configuration selection; path-base resolution; unknown-field rejection; duplicate profile IDs; store roots remain unchanged. |
| [Profiles](../specs/profiles.md) | Include order; cycle and missing-ID rejection; deduplication through multiple paths; fully qualified identity; target-collision rejection; deterministic ordering independent of input-map iteration. |
| [File Links](../specs/file-link.md) | Create, no-op, replace, relocate, remove, forget-missing, and managed identity-handoff outcomes; unmanaged-target protection; wrong-link and regular-file conflicts; parent-escape rejection; source and target containment; no parent removal. |
| [Lifecycle](../specs/lifecycle.md) | Every Desired/Known/Actual table row; blocked plans make no mutation; preflight failure creates no operation record; executor recheck rejects a target changed after planning; phase ordering; contiguous relocation; and stop-after-failure behavior. |
| [State and Recovery](../specs/state-and-recovery.md) | Corrupt-state rejection; canonical-hash fixtures; exclusive-lock contention; atomic-commit failure; every operation-status transition; same-source identity-handoff recovery; recovery to succeeded, failed, skipped, and uncertain; no rollback of verified earlier actions. |
| [CLI](../specs/cli.md) | Positional root-profile selection; `validate` default-profile and `--all` behavior; `diff` Known-to-Actual reporting and zero mutation; `plan` and `apply` default-profile behavior; confirmation after successful preflight and before an operation record; non-interactive `--yes` requirement; dry-run zero mutation; all documented exit-status classes. |

## Pure Domain Tests

Pure domain tests use resolved paths and typed observations only.
They must not parse YAML, access a store, inspect the host filesystem, acquire a lock, or serialize state.

At minimum, domain tests cover every row in the lifecycle transition table and assert both the action and its reason.
They also prove that resource ordering is stable when equivalent declarations are supplied in different mapping orders.
Canonical-hash fixtures use typed resolved values and assert the exact `definition_hash` and `desired_hash`, including deterministic resource ordering.
They prove that equal resolved definitions under distinct resource IDs have the same `definition_hash` and different `desired_hash` values.
Managed identity-handoff planning cases distinguish equal and differing resolved link targets and assert the recorded old and new identities and link targets.

Tests for a blocked plan must assert that the plan has blocking diagnostics and no executable action for the conflicting target.

## Filesystem Contract Tests

Filesystem contract tests run in a fresh temporary home, state directory, configuration directory, and local store.
They must never use the developer's real home directory, XDG directories, AppData directories, or a repository-owned state directory.

Each mutation test records the filesystem state before and after execution.
For a successful file-link operation, it asserts the final entry kind and normalized link target.
For a rejected operation, it asserts that the target, its parents, the store, and control files are unchanged.

The required negative cases include:

- a target regular file;
- a target link to a different source;
- a link that matches the desired source but lacks Known state;
- a missing, non-directory, symlinked, junction, or reparse-point parent;
- a source path that escapes or traverses an unexpected entry beneath the store root;
- a target outside the home root or inside a store or control path; and
- a replacement or removal whose actual link no longer matches Known state.

Tests that simulate a filesystem change between planning and execution must prove that the executor aborts rather than changing its planned action.

Managed identity-handoff tests cover both resolved-link-target cases.
When the targets are equal, they prove that the target is untouched, no replacement temporary path is allocated, and only the Known identity changes.
When the targets differ, they prove the Replace guarantees, including preservation of the old managed link on replacement failure.

## State Repository Durability Tests

State tests use controlled failures at each commit boundary: temporary-file creation, write, flush, parse, validation, replacement, and directory flush when available.
They assert that a failed commit leaves either the prior valid state or a recoverable active operation record; it must never leave a partial authoritative state.

Recovery tests construct an active operation record and real filesystem observations for each case:

| Recorded action result | Expected recovery |
| --- | --- |
| The recorded post-condition holds | Commit the matching Known-state update and mark the action succeeded. |
| The recorded precondition still holds | Mark the action failed without changing prior Known state. |
| A pending action was never started | Mark it skipped without changing Known state. |
| Neither condition can be proven, or observation is unsafe | Retain the operation as uncertain and block the next apply. |

Relocation recovery tests inject an interruption after the new link is verified and before the old link is removed.
They assert that recovery retains that partial relocation as `uncertain` without target mutation.

Same-source identity-handoff recovery tests interrupt a `running` action before its atomic state commit.
They assert that the old Known identity and shared expected link atomically recover to the new identity and `succeeded` without target mutation.

Lock tests require two independently created repository handles or processes.
They must prove that the second non-dry-run apply fails before target observation or mutation while the first holds the exclusive lock.

## Executor and CLI Tests

Executor integration tests exercise the complete sequence from resolved inputs through state commit.
They inject a filesystem or state failure after a mutation where necessary and assert the resulting operation record and target state.
They prove that no other action begins between a `relocate_link` action's verified new-link creation and verified old-link removal.

CLI acceptance tests invoke the compiled binary in an isolated environment.
They assert behavior rather than exact prose formatting.
For example, they check that a blocked plan identifies a conflict and exits with status `2`, not the precise English wording of that diagnostic.
Apply confirmation tests prove that prompting follows successful preflight and that a declined or unavailable confirmation leaves no operation record or target mutation.

`diff` acceptance tests construct Known state and expected, missing, wrong-link, other-entry, unsafe-parent, and unfinished-operation observations.
They assert that the command reports each category while leaving the target tree, state directory, store, configuration files, and operation record unchanged.
They also prove that `diff` neither needs nor reads a portable environment configuration.

Dry-run acceptance tests compare snapshots of the target tree, state directory, store, and control files before and after the command.
The snapshots must be identical.

## Platform Conformance

Platform-neutral tests may use a filesystem abstraction for deterministic failure injection, but they do not replace real platform evidence.

Unix coverage must exercise symbolic-link inspection without following the final link, a symlinked-parent rejection, atomic replacement of a managed link, and link-entry removal without touching the referent.
Windows coverage must exercise file symbolic-link behavior when available and reject junctions or unsupported reparse points.
It must also cover a replacement or removal rejected by access control or sharing when the test environment can create that condition, proving that no delete-then-create fallback and no premature Known-state update occur.
It must prove that a same-source managed identity handoff does not require replacement capability, while a source-changing handoff does require the documented replacement guarantee.
When the host cannot create a file symbolic link or cannot provide the required replacement guarantee, the test must prove the documented preflight failure rather than silently skipping the behavior.
Replacement tests must cover interruption or failure after the action-local temporary link is created, proving that only the exact recorded temporary link may be cleaned up and that an unexpected or unremovable temporary entry leaves the action uncertain.

Platform-specific tests run only in disposable directories and must clean up only the directories they created.

## Change Checklist

Before a change is ready for review:

1. Link the changed behavior to its architecture or specification owner.
2. Add or update the required test-layer evidence from the contract matrix.
3. Include at least one negative test for every new mutation path.
4. Include a zero-mutation test for every new dry-run, validation, blocked-plan, or preflight-failure path.
5. Add platform evidence when a behavior depends on symbolic links, path normalization, locking, or replacement semantics.
6. Record validation commands that could not run; do not claim unrun checks passed.
