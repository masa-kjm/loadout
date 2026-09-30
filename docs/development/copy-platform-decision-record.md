# File-Copy Platform Primitive Decision Record

## Purpose

This record captures the implementation and binding decisions that native CI evidence must support for the file-copy publication contract.
It is a development evidence record, not a specification; [File Copies](../specs/file-copy.md) and [Testing Strategy](testing.md) remain authoritative.

Each entry is revised only with its native CI result, including the runner OS/version, filesystem, Rust toolchain, command, and applicable test outcome.
The platform artifact includes `copy-capability-matrix.json` and its referenced focused-test logs; every matrix record names one action, capability decision, phase, command, result, and fixture-root environment.
An API candidate is not an enabled capability until the executor, compiled binary, and recovery evidence required by the testing strategy exists.
For `create-no-replace`, the record must distinguish temporary no-replace publication from direct exclusive create and record collision preservation, partial-final-artifact aftermath, and post-mutation classification for the candidate actually selected.

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
| Linux/ext4 create | Retained-parent `create-no-replace` | `rustix::fs::renameat_with` with `RenameFlags::NOREPLACE` for staged publication | Selected and enabled | Keep native executor, CLI, post-effect, and recovery coverage current. |
| Linux/ext4 `replace_copy` | Fresh old-copy ownership proof, staged replacement, and exact new/old/uncertain classification | `rustix` retained-parent rename operations | Selected and enabled | Keep native executor, CLI, post-effect, and recovery coverage current. |
| Linux/ext4 copy relocate and remove | Verified new `create-no-replace`, then rechecked owned-old removal and exact aftermath | `rustix` retained-parent rename/remove operations | Selected and enabled | Keep native executor, CLI, post-effect, and recovery coverage current. |
| Linux/ext4 link replacement | Preserved old expected effect on failed publication | `rustix` retained-parent rename operations | Selected and enabled | Keep native executor, CLI, post-effect, and recovery coverage current. |
| Linux/ext4 link/copy handoff | Preserved old expected target and complete effect aftermath | `rustix` retained-parent rename operations | Evidence incomplete; not release-enabled | Add compiled-binary native handoff evidence before publishing either handoff capability as enabled. |
| macOS/APFS create | Retained-parent `create-no-replace` plus verified APFS capability | staged retained-parent rustix renameat_with RENAME_EXCL; retained-FD libc APFS and VOL_CAP_INT_RENAME_EXCL query | Enabled only after the per-parent APFS/capability check | macOS CI records success, collision, post-effect, recovery, and compiled-binary results. |
| macOS/APFS replace, relocate, and remove | Retained-parent replacement/removal with action-specific aftermath | retained-parent rustix renameat/unlinkat after APFS/exclusive-rename capability query | Enabled only after each action's independent preflight check | macOS CI records copy-replacement classification, removal aftermath, executor, compiled-binary, and recovery evidence. |
| macOS/APFS link/copy handoff | Preserved old expected target and complete effect aftermath | No enabled primitive in this step | Fail-closed | Dedicated compiled-binary success/failure-aftermath/recovery evidence remains required. |
| Windows/NTFS create | `create-no-replace`, retained-parent/declared-path association, and exact classification | Temporary no-replace publication candidate using handle-relative `NtSetInformationFile` returned `os error 87`; direct exclusive create is unselected | Unresolved and fail-closed | Compare corrected temporary no-replace publication and direct exclusive create on native NTFS for success, collision preservation, reparse, sharing/ACL, write/flush/close/verify failure, post-error observation, and recovery. |
| Windows/NTFS `replace_copy` | Fresh old-copy ownership proof, staged replacement, and exact new/old/uncertain classification | No selected primitive; `ReplaceFileW` remains excluded | Unresolved and fail-closed | A separately reviewed native spike proving success, old-copy definite failure, uncertain missing/different/unsafe/unavailable aftermath, recovery, and no delete-then-create or backup/restore fallback. |
| Windows/NTFS relocate and source-changing handoff | Their independently required old-effect guarantees | No selected primitive | Unresolved and fail-closed | Do not infer enablement from Windows create or `replace_copy`; prove each action's required aftermath separately. |
| Windows/NTFS remove and same-source handoff | Rechecked no-follow removal or state-only identity transition | No selection in this record | Unresolved and fail-closed where current capability gating requires it | Retained-parent/no-follow, sharing and ACL denial, executor/CLI, and recovery evidence. |

The macOS and Windows entries are release work, not permitted permanent exclusions from the intended baseline.
The macOS gate is queried through the retained parent descriptor, never an absolute-path capability lookup. When APFS or exclusive-rename proof is unavailable, preflight rejects before operation creation, target mutation, or Known-state update. Windows and every action still marked fail-closed retain that rejection behavior.
