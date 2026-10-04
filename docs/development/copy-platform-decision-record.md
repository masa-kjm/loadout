# File-Copy Platform Primitive Decision Record

## Purpose

This record captures the implementation and binding decisions that native CI evidence must support for the file-copy publication contract.
It is a development evidence record, not a specification; [File Copies](../specs/file-copy.md) and [Testing Strategy](testing.md) remain authoritative.
It is the implementation-tracking record for the v0.5 copy actions and replaces a separate resumption plan: update the relevant action row with its primitive decision, required evidence, and enablement status as work progresses.

Each entry is revised only with its native CI result, including the runner OS/version, filesystem, Rust toolchain, command, and applicable test outcome.
The platform artifact includes `copy-capability-matrix.json` and its referenced focused-test logs; every matrix record names one action, capability decision, phase, command, result, and fixture-root environment.
An API candidate is not an enabled capability until the executor, compiled binary, and recovery evidence required by the testing strategy exists.
For `create-no-replace`, the record must distinguish temporary no-replace publication from direct exclusive create and record collision preservation, partial-final-artifact aftermath, and post-mutation classification for the candidate actually selected.

## Binding policy

Use the existing narrow bindings when they can express the exact contract:

- `rustix` for Unix retained-parent operations;
- `libc` only for Darwin capability queries or constants not exposed by `rustix`; and
- `windows-sys` for documented Win32 calls.

Do not add a high-level copy or move library, a generic cross-platform filesystem abstraction, path re-resolution, backup/restore, or an untracked fallback.
The only permitted sequential replacement is the specification-defined verified staging, owned-old removal, and no-replace final publication for copy replacement and effect handoff.
A new narrow binding or direct FFI declaration needs an API review, maintenance review, and native success, collision, failure-aftermath, and recovery evidence before it can enable an action.

## Current decisions

| Platform/action | Required property | Candidate and binding | Decision | Evidence still required before enablement |
| --- | --- | --- | --- | --- |
| Linux/ext4 create | Retained-parent `create-no-replace` | Candidate-shaped `rustix::fs::renameat_with` with `RenameFlags::NOREPLACE` for staged publication | Phase 7A candidate execution only; not selected or release-enabled | Native CI artifact for the exact candidate commit, final gate rerun, and review must select and enable this action. |
| Linux/ext4 `replace_copy` | Verified staging, fresh old-copy proof, removal, no-replace publication, and new/old/missing/different classification | Candidate-shaped `rustix` retained-parent remove and no-replace publication operations | Fail-closed pending its action-specific Phase 7A batch | Native safety, denial, aftermath, recovery, and compiled-binary evidence must select and enable this action. |
| Linux/ext4 copy relocate and remove | Verified new `create-no-replace`, then rechecked owned-old removal and exact aftermath | Candidate-shaped `rustix` retained-parent rename/remove operations | Fail-closed pending their action-specific Phase 7A batches | Native safety, denial, aftermath, recovery, and compiled-binary evidence must select and enable each action independently. |
| Linux/ext4 link replacement | Preserved old expected effect on failed publication | `rustix` retained-parent rename operations | Selected and enabled | Keep native executor, CLI, post-effect, and recovery coverage current. |
| Linux/ext4 link/copy handoff | Verified final-effect preparation, fresh old-effect proof, removal, destination-type no-replace publication, and complete effect aftermath | Candidate-shaped retained-parent operations for each destination type | Fail-closed pending their action-specific Phase 7A batches | Native safety, denial, aftermath, recovery, and compiled-binary evidence must select and enable either handoff capability. |
| macOS/APFS create | Retained-parent `create-no-replace` plus verified APFS capability | Staged retained-parent `rustix::fs::renameat_with` with `RenameFlags::NOREPLACE`; retained-FD libc APFS and `VOL_CAP_INT_RENAME_EXCL` query | Selected and enabled | Keep APFS native collision preservation, executor, recovery, compiled CLI, and capability-rejection coverage current. |
| macOS/APFS replace, relocate, and remove | Retained-parent sequential copy publication or removal with action-specific aftermath | Candidate-shaped retained-parent rustix renameat/unlinkat after APFS/exclusive-rename capability query | Fail-closed pending the Phase 7 evidence batch | Native filesystem, executor/state-recovery, compiled-binary CLI, and recovery evidence must select and enable each action independently. |
| macOS/APFS link/copy handoff | Verified preparation, owned-old removal, destination-type no-replace publication, and complete effect aftermath | No enabled primitive in this step | Fail-closed | Dedicated compiled-binary success/failure-aftermath/recovery evidence remains required. |
| Windows/NTFS create | `create-no-replace`, retained-parent/declared-path association, and exact classification | Direct exclusive `NtCreateFile(FILE_CREATE)` and temporary publication through handle-relative `NtSetInformationFile(FileRenameInformation)` | Partial native spike evidence; executor remains fail-closed | Add sharing denial, flush and close failure, post-error classification, state/recovery, executor, and CLI evidence before selecting or enabling either implementation. |
| Windows/NTFS `replace_copy` | Verified staging, fresh old-copy proof, removal, no-replace publication, and new/old/missing/different classification | No selected primitive; `ReplaceFileW` remains excluded | Unresolved and fail-closed | A separately reviewed native spike proving success, old-copy and missing definite failure, different-entry conflict, unsafe/unavailable uncertainty, recovery, and no overwrite, backup, or restoration. |
| Windows/NTFS relocate and source-changing handoff | Their independently required verified-new, fresh-old-removal, and destination-publication aftermath | No selected primitive | Unresolved and fail-closed | Do not infer enablement from Windows create or `replace_copy`; prove each action's required aftermath separately. |
| Windows/NTFS remove and same-source handoff | Rechecked no-follow removal or state-only identity transition | No selection in this record | Unresolved and fail-closed where current capability gating requires it | Retained-parent/no-follow, sharing and ACL denial, executor/CLI, and recovery evidence. |

## Phase 2 local native spike (partial; Step 2 incomplete)

On 2026-09-30, the Windows-native probe ran on Windows 11 build 26200.0 with PowerShell 5.1.26100.9549, NTFS `%TEMP%`, and Rust 1.95.0.
The command was `cargo.exe test --test native_copy_platform -- --nocapture` from the WSL UNC worktree, with `CARGO_TARGET_DIR` set to a unique child of `%TEMP%` and fixtures below `%TEMP%`.
This partial spike does not complete Step 2 and selects neither candidate. It does not authorize direct-create recovery or diagnostic behavior, executor integration, or Windows copy capability enablement.

### Candidate matrix

| Required native condition | Direct exclusive create | Temporary no-replace publication |
| --- | --- | --- |
| Success and exact bytes | Confirmed | Confirmed; temporary missing after publication |
| Existing-final collision preservation | Confirmed | Confirmed; final and temporary preserved |
| Final file-reparse rejection | Confirmed | Confirmed; link and referent preserved |
| Parent AddFile ACL denial | Confirmed; final remains missing | Confirmed; temporary remains missing |
| Publication ACL denial after staging | Not applicable | Confirmed; `ERROR_ACCESS_DENIED`, temporary preserved, final missing |
| Non-empty partial write failure | Confirmed; final retains the flushed prefix | Confirmed; temporary retains the flushed prefix and final is missing |
| Verification failure | Confirmed for locked read and byte mismatch | Confirmed for locked read and byte mismatch before publication |
| Post-mutation missing-name observation | Confirmed; final observation is `NotFound` | Confirmed; publication errors and both names are missing |
| Sharing denial | Unverified | Unverified |
| Flush failure | Unverified | Unverified |
| Close failure | Unverified | Unverified |

- Retained-parent direct exclusive `NtCreateFile(FILE_CREATE)` created and flushed exact bytes and rejected an existing final name without changing its bytes. A separate successful write followed by an ordinary close left its bytes at the final pathname; it is only an artifact observation, not an injected write, flush, close, or verification failure.
- Retained-parent temporary publication used `NtSetInformationFile` with `FileRenameInformation`, a zero `ReplaceIfExists` field, and `DELETE` access on the temporary handle. It published to a missing final name and rejected an existing final name with `ERROR_ALREADY_EXISTS` while preserving both the existing final bytes and the temporary bytes.
- Both candidates rejected a file reparse point at the final declared path without changing the link or its referent.
- A disposable parent-directory DACL that denies the current principal `WD` caused both candidates’ initial create to return `ERROR_ACCESS_DENIED`; neither the direct final nor the staged temporary name appeared. The same denial after temporary staging caused handle-relative publication to return `ERROR_ACCESS_DENIED`, preserving the temporary bytes and leaving the final name missing. The fixture removed that explicit deny ACE before cleanup.
- After each candidate flushed a non-empty prefix, an exclusive byte-range lock on the next byte caused `WriteFile` through the candidate handle to return `ERROR_LOCK_VIOLATION`. After lock release, the direct final and staged temporary retained only their flushed prefix bytes, and the staged final name was still missing.
- An exclusive byte-range lock also caused direct-final and staged-temporary verification reads to return `ERROR_LOCK_VIOLATION`. After lock release, each artifact retained its planned bytes and the staged final name remained missing.
- A separate writer handle changed an already flushed direct final and a staged temporary before their byte verification. The observed bytes differed from the planned bytes; the staged final name remained missing.
- A separate filesystem operation removed a written direct final and a written staged temporary while the candidate handles remained open. The direct final path observation returned `NotFound`; handle-relative staged publication returned an error, and both staged names remained missing.
- The earlier `FileRenameInformationEx` candidate and a temporary handle without `DELETE` access are not selected; they produced `ERROR_INVALID_PARAMETER` and `ERROR_ACCESS_DENIED` respectively during the same spike.

This is primitive-only evidence. Sharing denial, flush and close failure, post-error classification, recovery, executor, and compiled-binary evidence remain unverified. The Windows copy executor therefore remains fail-closed.

The macOS and Windows entries are release work, not permitted permanent exclusions from the intended baseline.
The macOS gate is queried through the retained parent descriptor, never an absolute-path capability lookup. When APFS or exclusive-rename proof is unavailable, preflight rejects before operation creation, target mutation, or Known-state update. Windows and every action still marked fail-closed retain that rejection behavior.

## Phase 7 local evidence batch (2026-10-01; no capability enabled)

All records in this batch retain the runtime `fail_closed` decision.
The action-level matrix records candidate executor and recovery checks separately from compiled-binary rejection; a passing candidate check is not a publication capability selection.

### Linux/ext4 local batch

The batch ran on Linux 5.15.167.4-microsoft-standard-WSL2 with an ext4 disposable fixture at `/tmp/loadout-phase7-linux-evidence.FC3IRG`, Rust 1.95.0, and Cargo 1.95.0.
The command was `LOADOUT_NATIVE_FIXTURE_ROOT=/tmp/loadout-phase7-linux-evidence.FC3IRG TMPDIR=/tmp/loadout-phase7-linux-evidence.FC3IRG TEMP=/tmp/loadout-phase7-linux-evidence.FC3IRG TMP=/tmp/loadout-phase7-linux-evidence.FC3IRG python3 .github/scripts/run-copy-capability-matrix.py`.
The generated `copy-capability-matrix.json` contained 24 passing records for `create_copy`, `replace_copy`, `remove_copy`, `relocate_copy`, `link_to_copy_handoff`, and `copy_to_link_handoff`; every record matched exactly one passing test and named capability `fail_closed`.
The matrix covered retained-parent candidate executor postconditions, action-specific recovery/post-effect classification, and compiled-binary fail-closed rejection without a new operation record or target mutation.
It did not run compiled-binary publication success, native sharing/ACL denial, complete post-error aftermath, or manual-correction recovery for an enabled action.
No Linux action is selected or enabled from this batch.

### Windows/NTFS local batch

The batch ran on Windows version 2009 with PowerShell 5.1.26100.9549, NTFS disposable fixtures below `%TEMP%`, Rust 1.95.0, and Cargo 1.95.0 from the WSL UNC worktree.
The Windows-native direct invocation used PowerShell `-NoProfile` with this complete context:

```powershell
Set-Location '\\wsl.localhost\Ubuntu-24.04\home\masal\ghq\github.com\masa-kjm\loadout'
$toolchain = 'C:\Users\ilafm\AppData\Local\mise\installs\rust\1.95.0'
$env:PATH = "$toolchain;$env:PATH"
$env:TEMP = 'C:\Users\ilafm\AppData\Local\Temp'
$env:TMP = 'C:\Users\ilafm\AppData\Local\Temp'
$env:CARGO_TARGET_DIR = 'C:\Users\ilafm\AppData\Local\Temp\loadout-phase7-windows-direct-20261001'
$env:LOADOUT_NATIVE_FIXTURE_ROOT = 'C:\Users\ilafm\AppData\Local\Temp\loadout-phase7-windows-direct-fixtures-20261001'
& "$toolchain\cargo.exe" test --test native_copy_platform
```

The native command passed 10 primitive-probe tests, including direct and staged collision preservation, file-reparse rejection, parent ACL denial, partial-write aftermath, verification observation, and temporary-publication aftermath.
The compiled-binary commands `cargo.exe test --test cli_read_only copy_capability_preflight_rejection_creates_no_operation_or_target` and `cargo.exe test --test cli_read_only copy_replace_relocate_and_remove_fail_preflight_without_mutating_target_or_state` passed, proving fail-closed rejection for create, replace, relocate, and remove without a new operation record or target mutation.
The state commands `cargo.exe test recovery_preserves_copy_replacement_until_a_failed_known_commit_can_be_retried` and `cargo.exe test recovery_commits_or_fails_copy_relocation_from_recorded_conditions` passed.
The local Windows environment exposes only the Microsoft Store `python.exe` launcher and no Python runtime, so `.github/scripts/run-copy-capability-matrix.py` was unrun on Windows; no Windows matrix artifact was claimed.
Windows sharing denial, flush and close failure, selected executor integration, compiled-binary publication success, and file-symbolic-link handoff policy evidence remain unrun.
No Windows action is selected or enabled from this batch.

### macOS/APFS

No macOS/APFS runner was available for this batch.
Every macOS action remains fail-closed pending its retained-parent APFS capability check and the required native, executor/state-recovery, and compiled-binary evidence.

## Phase 7B macOS/APFS evidence and selection (2026-10-03)

CI run [#75](https://github.com/masa-kjm/loadout/actions/runs/37130136937) passed for candidate commit `037b6646639b689b524100a2731b91ce3dd9657b`.
The `macos-latest` runner was macOS 26.6.2 build 25G83 on APFS at `/Users/runner/work/_temp`, with Rust and Cargo 1.95.0 on `aarch64-apple-darwin`.
Its `cargo test --locked`, `cargo test --locked --test native_copy_platform`, and compiled-binary `copy_lifecycle_commands_render_typed_actions_and_apply_on_apfs` checks all passed.
The compiled CLI test proved retained-parent APFS capability query, `VOL_CAP_INT_RENAME_EXCL` availability, exact copy creation, Known-state commit, and no active operation after success.
The same artifact retains fail-closed preflight rejection evidence for `replace_copy`, `remove_copy`, `relocate_copy`, and both effect handoffs.

CI run [#76](https://github.com/masa-kjm/loadout/actions/runs/37131780844) passed for the matrix follow-up commit `370c0124ef72f3905b4858ad6a58582e0f455ead` on the same macOS/APFS and Rust toolchain.
Its action-level matrix recorded one passing test for each `create_copy` requirement: native collision preservation, executor postcondition, uncertain-create recovery, compiled CLI success, and non-APFS capability rejection.
The runtime gate, decision record, and next matrix revision now use the shared `selected_and_enabled` status for macOS/APFS `create_copy`.
The final CI run for this selection revision remains required to prove that status and the record are present at the exact enabled revision.

## Phase 7A local Linux candidate batch (2026-10-02; not release-enabled)

This batch executes the Linux candidate paths in an unmerged Phase 7A worktree.
It does not select or enable a release capability, and the candidate must remain out of the release branch until an Ubuntu CI artifact for its exact commit is reviewed.

The batch ran on Linux 5.15.167.4-microsoft-standard-WSL2 with an ext4 disposable fixture at `/tmp/loadout-phase7a-linux-evidence.2VSU4p`, Rust 1.95.0, and Cargo 1.95.0.
The command was `LOADOUT_NATIVE_FIXTURE_ROOT=/tmp/loadout-phase7a-linux-evidence.2VSU4p TMPDIR=/tmp/loadout-phase7a-linux-evidence.2VSU4p TEMP=/tmp/loadout-phase7a-linux-evidence.2VSU4p TMP=/tmp/loadout-phase7a-linux-evidence.2VSU4p python3 .github/scripts/run-copy-capability-matrix.py`.
The generated matrix contained nine passing records, each with exactly one matched and one passing test.
Four `candidate` records cover retained-parent `create_copy` executor postconditions, compiled-binary success with an exact Known-state commit, and recorded-condition recovery/post-effect classification.
Five `fail_closed` records cover preflight rejection without target mutation or a new operation record for `replace_copy`, `relocate_copy`, `remove_copy`, and both handoffs.
The focused native probe passed two Linux tests for temporary no-replace success with exact bytes and collision preservation.

The local candidate batch does not replace the required Ubuntu CI artifact, final gate rerun, or review for the exact candidate commit.
It also does not prove the native denial and post-error conditions needed for `create_copy` release enablement, or any native safety, denial, or aftermath conditions for the still fail-closed Linux actions, nor any macOS or Windows capability.
No Linux action is selected or enabled from this batch.
