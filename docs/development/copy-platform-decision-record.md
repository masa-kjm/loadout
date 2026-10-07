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

| Platform/action | Required property | Candidate and binding | Decision | Evidence to keep current |
| --- | --- | --- | --- | --- |
| Linux/ext4 create | Retained-parent `create-no-replace` | `rustix::fs::renameat_with` with `RenameFlags::NOREPLACE` for staged publication | Selected and enabled | Keep native collision preservation, executor, recovery, compiled CLI, and capability-rejection coverage current. |
| Linux/ext4 `replace_copy` | Verified staging, fresh old-copy proof, removal, no-replace publication, and new/old/missing/different classification | `rustix` retained-parent remove and no-replace publication operations | Selected and enabled | Keep native safety, denial, aftermath, recovery, and compiled-binary evidence current. |
| Copy `replace_ownership` with changed bytes | The `replace_copy` primitive plus atomic old-to-new Known identity update | No additional filesystem primitive | Selected and enabled on Linux/ext4, macOS/APFS, and Windows/NTFS | Keep changed-fingerprint handoff state, aftermath, recovery, and compiled-binary evidence current. |
| Linux/ext4 copy relocate and remove | Verified new `create-no-replace`, then rechecked owned-old removal and exact aftermath | `rustix` retained-parent rename/remove operations | Selected and enabled | Keep native safety, denial, aftermath, recovery, and compiled-binary evidence current. |
| Linux/ext4 link replacement | Preserved old expected effect on failed publication | `rustix` retained-parent rename operations | Selected and enabled | Keep native executor, CLI, post-effect, and recovery coverage current. |
| Linux/ext4 link/copy handoff | Verified final-effect preparation, fresh old-effect proof, removal, destination-type no-replace publication, and complete effect aftermath | Retained-parent operations for each destination type | Selected and enabled | Keep native safety, denial, aftermath, recovery, and compiled-binary evidence current. |
| macOS/APFS create | Retained-parent `create-no-replace` plus verified APFS capability | Staged retained-parent `rustix::fs::renameat_with` with `RenameFlags::NOREPLACE`; retained-FD libc APFS and `VOL_CAP_INT_RENAME_EXCL` query | Selected and enabled | Keep APFS native collision preservation, executor, recovery, compiled CLI, and capability-rejection coverage current. |
| macOS/APFS replace, relocate, and remove | Retained-parent sequential copy publication or removal with action-specific aftermath | Retained-parent `rustix` renameat/unlinkat after the APFS/exclusive-rename capability query | Selected and enabled | Keep native filesystem, executor/state-recovery, compiled-binary CLI, and recovery evidence current. |
| macOS/APFS link/copy handoff | Verified preparation, owned-old removal, destination-type no-replace publication, and complete effect aftermath | Retained-parent operations for each destination type after the APFS capability query | Selected and enabled | Keep compiled-binary success, failure-aftermath, and recovery evidence current. |
| Windows/NTFS create | `create-no-replace`, retained-parent/declared-path association, and exact classification | Direct exclusive `NtCreateFile(FILE_CREATE)` and temporary publication through handle-relative `NtSetInformationFile(FileRenameInformation)` | Selected and enabled | Keep sharing denial, flush and close failure, post-error classification, state/recovery, executor, and CLI evidence current. |
| Windows/NTFS `replace_copy` | Verified staging, fresh old-copy proof, removal, no-replace publication, and new/old/missing/different classification | `windows-sys` retained-parent staging, owned removal, and no-replace publication; `ReplaceFileW` remains excluded | Selected and enabled | Keep native denial and aftermath, recovery, no-overwrite, no-backup, and no-restoration evidence current. |
| Windows/NTFS relocate and source-changing handoff | Their independently required verified-new, fresh-old-removal, and destination-publication aftermath | Retained-parent primitives for each destination type | Selected and enabled | Keep the action-specific denial, aftermath, recovery, and compiled-binary evidence current. |
| Windows/NTFS remove and same-source handoff | Rechecked no-follow removal or state-only identity transition | Retained-parent no-follow removal or destination create | Selected and enabled | Keep retained-parent/no-follow, sharing and ACL denial, executor/CLI, and recovery evidence current. |

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

## Phase 7C candidate CI evidence review (2026-10-06; no further enablement)

CI run [#79](https://github.com/masa-kjm/loadout/actions/runs/37461587411) passed for candidate commit `833193dda2038ff086c7189907cda11c16d16c90`.
All Ubuntu, macOS, and Windows jobs passed `cargo fmt --all -- --check`, `cargo clippy --all-targets --locked -- -D warnings`, the platform's locked test suite, the native copy primitive probe, and the action-level matrix.
The uploaded artifacts record each command and focused-test result below.

### Linux/ext4

The Ubuntu runner used Linux `6.17.0-1022-azure`, ext4 at `/home/runner/work/_temp`, and Rust/Cargo `1.95.0` on `x86_64-unknown-linux-gnu`.
The matrix confirmed the existing `create_copy` candidate executor, recovery, and compiled-binary-success records.
For `replace_copy`, it additionally confirmed native retained-parent no-replace collision preservation and publication, plus candidate executor and recovery records.
Candidate executor and recovery records passed for `remove_copy`, `relocate_copy`, `link_to_copy_handoff`, and `copy_to_link_handoff`.
The compiled-binary checks for every non-create action intentionally remained preflight rejection checks.

This is not selection evidence for a sequential action: it lacks an enabled compiled-binary success path and the action-specific native denial and post-error aftermath required by the decision rows.
All Linux actions other than the existing candidate `create_copy` remain fail-closed.

### macOS/APFS

The `macos-latest` runner used macOS `26.6.2` build `25G83`, APFS at `/Users/runner/work/_temp`, and Rust/Cargo `1.95.0` on `aarch64-apple-darwin`.
The matrix reconfirmed all five selected-and-enabled `create_copy` records from Phase 7B.
Candidate executor and recovery records passed for `replace_copy`, `remove_copy`, `relocate_copy`, and both effect handoffs, while their compiled-binary checks intentionally remained preflight rejection checks.

No additional macOS action is selected: native action-specific failure-aftermath and compiled-binary success evidence are still required.
The existing macOS/APFS `create_copy` enablement remains unchanged.

### Windows/NTFS

The `windows-latest` runner used Windows version `2009`, NTFS at `D:\\a\\_temp`, and Rust/Cargo `1.95.0` on `x86_64-pc-windows-msvc`.
The direct exclusive-create primitive candidate passed its exact-byte and collision test, and every copy action's compiled-binary preflight-rejection test passed where applicable.
The matrix deliberately recorded `unrun` native-executor candidates for `replace_copy`, `relocate_copy`, and both effect handoffs; the handoffs also require a file-symbolic-link fixture.

Windows therefore remains fail-closed for every copy action.
This run does not add the missing action-level native executor, denial, post-error classification, recovery, or compiled-binary success evidence required for selection.

## Phase 7D Windows candidate executor and recovery evidence (2026-10-06; no enablement)

CI run [#80](https://github.com/masa-kjm/loadout/actions/runs/37468598405) passed for candidate commit `b35fff98efbb53e169cc48df88a28d3dc08cfbcd`.
The `windows-latest` runner used Windows version `2009`, NTFS at `D:\\a\\_temp`, and Rust/Cargo `1.95.0` on `x86_64-pc-windows-msvc`.
Its locked suite, native primitive probe, and action-level matrix all passed.

The matrix now records passing Windows candidate executor and recorded-fact recovery tests for `replace_copy`, `remove_copy`, `relocate_copy`, `link_to_copy_handoff`, and `copy_to_link_handoff`.
The handoff fixture creates a real file symbolic link; it does not leave handoff evidence unrun when that capability is available on the runner.
For each of those actions, the compiled-binary record still proves only fail-closed preflight rejection without target or state mutation.

This evidence promotes the retained-parent implementations from unrun to candidate execution/recovery evidence only.
No Windows copy action is selected or enabled: action-specific native denial and post-error aftermath, enabled compiled-binary success, and the remaining recovery evidence in the decision rows are still required.

## Phase 7E cross-platform candidate compiled-binary evidence (2026-10-06; no enablement)

CI run [#81](https://github.com/masa-kjm/loadout/actions/runs/37472086534) passed for candidate commit `b5fc5998632a5b84a64ea5a0ea874127641d8474`.
Every job passed `cargo fmt --all -- --check`, `cargo clippy --all-targets --locked -- -D warnings`, its locked test suite, the native copy primitive probe, the action-level matrix, and `cargo test --locked --features copy-candidate-actions --test cli_read_only candidate_sequential_copy_actions_apply_and_commit_known_state`.

The candidate-only compiled-binary test applies `create_copy`, `replace_copy`, `relocate_copy`, `remove_copy`, `link_to_copy_handoff`, and `copy_to_link_handoff` in isolated fixtures, then verifies each final effect and Known-state commit.
It passed on Linux `6.17.0-1022-azure` with ext4 at `/home/runner/work/_temp` and Rust/Cargo `1.95.0` on `x86_64-unknown-linux-gnu`; macOS `26.6.2` build `25G83` with APFS at `/Users/runner/work/_temp` and Rust/Cargo `1.95.0` on `aarch64-apple-darwin`; and Windows version `2009` with NTFS at `D:\a\_temp` and Rust/Cargo `1.95.0` on `x86_64-pc-windows-msvc`.

At that commit, `copy-candidate-actions` was a CI-only compiler feature, not a CLI option or release capability: the ordinary build continued to preflight-reject every action not otherwise selected by its platform decision row.
Together with the native probe, action-level matrix, and ordinary compiled CLI evidence, this exact-commit final-gate run selects and enables Linux/ext4 `create_copy`.
Its ordinary runtime gate already uses the selected retained-parent `renameat_with(..., RenameFlags::NOREPLACE)` publication path.
The candidate-only success records for the remaining sequential actions did not select or enable those actions at that revision.

## Phase 7F cross-platform full enablement (2026-10-07)

CI run [#84](https://github.com/masa-kjm/loadout/actions/runs/37562624757) passed for commit `c27ae9695079136245ca9d6ebcc50a73e8ce1752`.
Every job passed `cargo fmt --all -- --check`, `cargo clippy --all-targets --locked -- -D warnings`, its locked test suite, the native copy primitive probe, the ordinary compiled-binary sequential copy-action evidence, and the action-level capability matrix.
The matrix recorded only passing `selected_and_enabled` entries: 21 on Linux/ext4, 20 on macOS/APFS, and 19 on Windows/NTFS.

The Ubuntu runner used Linux `6.17.0-1022-azure`, ext4 at `/home/runner/work/_temp`, and Rust/Cargo `1.95.0` on `x86_64-unknown-linux-gnu`.
The macOS runner used macOS `26.6.2` build `25G83`, APFS at `/Users/runner/work/_temp`, and Rust/Cargo `1.95.0` on `aarch64-apple-darwin`.
The Windows runner used Windows version `2009`, NTFS at `D:\a\_temp`, and Rust/Cargo `1.95.0` on `x86_64-pc-windows-msvc`.

The ordinary compiled-binary evidence applies `create_copy`, `replace_copy`, `relocate_copy`, `remove_copy`, `link_to_copy_handoff`, and `copy_to_link_handoff` in isolated fixtures, verifies their final effects and Known-state commits, and no longer uses a candidate-only compiler feature.
The runtime gate is therefore enabled for every file-copy action on the three baseline platform/filesystem combinations, while unsupported platforms remain fail-closed and macOS continues to require the retained-parent APFS capability query before each enabled action.
