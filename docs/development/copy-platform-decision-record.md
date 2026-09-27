# File-Copy Platform Primitive Decision Record

## Purpose

This record captures the implementation and binding decisions that native CI evidence must support for the file-copy publication contract.
It is a development evidence record, not a specification; [File Copies](../specs/file-copy.md) and [Testing Strategy](testing.md) remain authoritative.

Each entry is revised only with its native CI result, including the runner OS/version, filesystem, Rust toolchain, command, and applicable test outcome.
The platform artifact includes `copy-capability-matrix.json` and its referenced focused-test logs; every matrix record names one action, capability decision, phase, command, result, and fixture-root environment.
An API candidate is not an enabled capability until the executor, compiled binary, and recovery evidence required by the testing strategy exists.

## Binding policy

Use the existing narrow bindings when they can express the exact contract:

- `rustix` for Unix retained-parent operations;
- `libc` only for Darwin capability queries or constants not exposed by `rustix`; and
- `windows-sys` for documented Win32 calls.

Do not add a high-level copy or move library, a generic cross-platform filesystem abstraction, path re-resolution, or a delete-then-create fallback.
A new narrow binding or direct FFI declaration needs an API review, maintenance review, and native success, collision, failure-aftermath, and recovery evidence before it can enable an action.

## Current decisions

| Platform/action | Required property | Candidate and binding | Decision | Evidence still required before enablement |
| --- | --- | --- | --- | --- |
| Linux/ext4 create | Retained-parent no-replace publication | `rustix::fs::renameat_with` with `RenameFlags::NOREPLACE` | Selected and enabled | Keep native executor, CLI, post-effect, and recovery coverage current. |
| Linux/ext4 replace, relocate, and remove | Preserved old expected target on failed publication and exact aftermath | `rustix` retained-parent rename/remove operations | Selected and enabled | Keep native executor, CLI, post-effect, and recovery coverage current. |
| Linux/ext4 link/copy handoff | Preserved old expected target and complete effect aftermath | `rustix` retained-parent rename operations | Evidence incomplete; not release-enabled | Add compiled-binary native handoff evidence before publishing this capability as enabled. |
| macOS/APFS create | Retained-parent exclusive rename plus verified APFS capability | retained-parent rustix renameat_with RENAME_EXCL; retained-FD libc APFS and VOL_CAP_INT_RENAME_EXCL query | Enabled only after the per-parent APFS/capability check | macOS CI records success, collision, post-effect, recovery, and compiled-binary results. |
| macOS/APFS replace, relocate, and remove | Retained-parent atomic replacement or name-based removal and preserved old expected target on failed publication | retained-parent rustix renameat/unlinkat after APFS/exclusive-rename capability query | Enabled only after each action's independent preflight check | macOS CI records failure aftermath, executor, compiled-binary, and recovery evidence. |
| macOS/APFS link/copy handoff | Preserved old expected target and complete effect aftermath | No enabled primitive in this step | Fail-closed | Dedicated compiled-binary success/failure-aftermath/recovery evidence remains required. |
| Windows/NTFS create | No-replace publication plus retained-parent/declared-path association | `windows-sys::MoveFileExW` without `MOVEFILE_REPLACE_EXISTING` | Collision candidate only; not selected for production | NTFS association proof, reparse checks, executor/CLI/recovery evidence, and proof that the call cannot replace the final name. |
| Windows/NTFS replace, relocate, and source-changing handoff | Atomic replacement that preserves the old expected target when publication fails | No selected primitive; `ReplaceFileW` remains excluded by the specification | Unresolved and fail-closed | A separately reviewed native primitive spike proving success, failure aftermath, recovery, and no delete-then-create fallback. |
| Windows/NTFS remove and same-source handoff | Rechecked no-follow removal or state-only identity transition | No selection in this record | Unresolved and fail-closed where current capability gating requires it | Retained-parent/no-follow, sharing and ACL denial, executor/CLI, and recovery evidence. |

The macOS and Windows entries are release work, not permitted permanent exclusions from the intended baseline.
The macOS gate is queried through the retained parent descriptor, never an absolute-path capability lookup. When APFS or exclusive-rename proof is unavailable, preflight rejects before operation creation, target mutation, or Known-state update. Windows and every action still marked fail-closed retain that rejection behavior.
