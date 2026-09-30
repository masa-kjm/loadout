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
| Windows/NTFS create | `create-no-replace`, retained-parent/declared-path association, and exact classification | Direct exclusive `NtCreateFile(FILE_CREATE)` and temporary publication through handle-relative `NtSetInformationFile(FileRenameInformation)` | Partial native spike evidence; executor remains fail-closed | Add reparse, sharing/ACL, write/flush/close/verify failure, post-error observation, state/recovery, executor, and CLI evidence before selecting or enabling either implementation. |
| Windows/NTFS `replace_copy` | Fresh old-copy ownership proof, staged replacement, and exact new/old/uncertain classification | No selected primitive; `ReplaceFileW` remains excluded | Unresolved and fail-closed | A separately reviewed native spike proving success, old-copy definite failure, uncertain missing/different/unsafe/unavailable aftermath, recovery, and no delete-then-create or backup/restore fallback. |
| Windows/NTFS relocate and source-changing handoff | Their independently required old-effect guarantees | No selected primitive | Unresolved and fail-closed | Do not infer enablement from Windows create or `replace_copy`; prove each action's required aftermath separately. |
| Windows/NTFS remove and same-source handoff | Rechecked no-follow removal or state-only identity transition | No selection in this record | Unresolved and fail-closed where current capability gating requires it | Retained-parent/no-follow, sharing and ACL denial, executor/CLI, and recovery evidence. |

## Phase 2 local native spike (partial; Step 2 incomplete)

On 2026-09-30, the Windows-native probe ran on Windows 11 build 26200.0 with PowerShell 5.1.26100.9549, NTFS `%TEMP%`, and Rust 1.95.0.
The command was `cargo.exe test --test native_copy_platform -- --nocapture` from the WSL UNC worktree, with `CARGO_TARGET_DIR` set to a unique child of `%TEMP%` and fixtures below `%TEMP%`.
This partial spike does not complete Step 2 and selects neither candidate. It does not authorize direct-create recovery or diagnostic behavior, executor integration, or Windows copy capability enablement.

- Retained-parent direct exclusive `NtCreateFile(FILE_CREATE)` created and flushed exact bytes, rejected an existing final name without changing its bytes, and left a deliberately incomplete final-path artifact intact after close.
- Retained-parent temporary publication used `NtSetInformationFile` with `FileRenameInformation`, a zero `ReplaceIfExists` field, and `DELETE` access on the temporary handle. It published to a missing final name and rejected an existing final name with `ERROR_ALREADY_EXISTS` while preserving both the existing final bytes and the temporary bytes.
- The earlier `FileRenameInformationEx` candidate and a temporary handle without `DELETE` access are not selected; they produced `ERROR_INVALID_PARAMETER` and `ERROR_ACCESS_DENIED` respectively during the same spike.

This is primitive-only evidence. Reparse-point rejection, sharing denial, ACL denial, actual write/flush/close/verification failure, post-error classification, recovery, executor, and compiled-binary evidence remain unverified. The Windows copy executor therefore remains fail-closed.

The macOS and Windows entries are release work, not permitted permanent exclusions from the intended baseline.
The macOS gate is queried through the retained parent descriptor, never an absolute-path capability lookup. When APFS or exclusive-rename proof is unavailable, preflight rejects before operation creation, target mutation, or Known-state update. Windows and every action still marked fail-closed retain that rejection behavior.
