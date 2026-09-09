# File-Link Resource Specification

## Scope

This specification defines the only v0.2.0 resource implementation: a regular file from a local store materialized as a file symbolic link below the current user's home directory.
It defines declaration syntax, containment, ownership, observation, mutation preconditions, and platform requirements.

Copy operations, directory resources, hard links, junctions, and remote stores are outside v0.2.0.

## External Filesystem Concurrency

Loadout requires recorded ownership and a fresh no-follow observation of the expected link before removal or replacement. It rechecks the applicable source, target, temporary, parent and containment conditions immediately before each mutation step and rejects changes it observes. Loadout does not synchronize with or exclude external filesystem changes by editors, synchronization tools, or other processes, including adversarial processes. Changes between the last recheck and the filesystem operation can cause a substituted unmanaged entry to be deleted or replaced. A retained parent handle can still refer to a directory that another process has moved; it does not guarantee continuous containment below home. Loadout verifies the required recorded postconditions and path association before committing success, but these observations are not an atomic snapshot. In particular, a missing target after removal can lead to `succeeded`, removal of Known state and exit 0 without proving which entry was deleted. Loadout does not guarantee detection of every race, reversal of unintended effects, or repair of external changes. Users are not required to prevent external changes for Loadout to perform its safety checks.

These limits apply equally to final targets, recorded replacement temporaries, source-path observations, relevant parents and ancestors, and recovery cleanup. External inactivity is neither a safety precondition nor evidence that an action is safe. The state repository lock serializes cooperating Loadout sessions; it does not lock managed targets against unrelated processes.

A retained parent handle binds an operation to a directory object, not continuously to its declared path or location below home. The executor MUST check the association with the recorded path and canonical home at immediate recheck and post-observation boundaries. A handle-local postcondition alone MUST NOT establish success for a different declared path. Required facts that cannot be established leave the result uncertain. No atomic compare-and-delete or uninterrupted physical-containment guarantee is provided between observations and syscalls.

The ownership and containment requirements below apply at their specified observation boundaries, subject to this concurrency limit; they do not guarantee the identity of an entry substituted afterward.

## Declaration

A file-link resource has `type: file` and the following properties:

```yaml
type: file
properties:
  kind: file
  source:
    store: dotfiles
    path: git/config
  target: ~/.gitconfig
  operation: link
```

All fields shown above are REQUIRED.
Unknown fields are errors.

`kind` MUST be `file`.
`operation` MUST be `link`.
No default value is implied for either field.

## Source Resolution

`source.store` identifies a `local` store declared in the environment configuration.
`source.path` is a non-empty relative path written with `/` separators.
It MUST NOT be absolute, begin with `~/`, contain an empty component, contain `.` or `..`, or include a platform path prefix.

The source resolves from the physical store root.
Every existing component from that root to the source parent MUST be a directory and MUST NOT be a symlink, junction, or reparse point.
The final source entry MUST be an existing regular file and MUST NOT be a symlink, junction, or reparse point.

The resolved source path is therefore physically contained by the store root.
If containment or entry-kind verification fails, validation fails and no target is inspected or modified.

## Target Resolution

`target` MUST be an absolute path or begin with `~/`.
It MUST NOT contain `.` or `..` path components after home expansion.
Resolution produces an absolute target candidate below the current user's home directory.

Before target observation or mutation, the inspector and executor MUST prove that the target is physically contained by the current user's canonical home directory.
All target parent directories MUST already exist.
From the canonical home directory to the target parent, each component MUST be a directory and MUST NOT be a symlink, junction, or reparse point.
Loadout does not create target parent directories in v0.2.0.

The resolved target MUST NOT:

- equal the resolved source;
- be inside a local store root;
- equal the runtime configuration file, environment configuration file, or a discovered profile file; or
- be inside the runtime configuration directory or state directory.

These checks protect source assets and Loadout control data from resource mutation.

## Link Representation

Loadout creates an absolute file symbolic link whose target is the verified resolved source path.
The exact normalized link target is recorded in Known state after post-condition verification.

Loadout does not create relative links, hard links, directory links, junctions, or other reparse points in v0.2.0.

## Observation

Observation uses no-follow metadata for the target and every existing target parent.
For a resource whose target parents are safe, the inspector classifies the final target as one of:

| Observation | Meaning |
| --- | --- |
| `missing` | No entry exists at the target. |
| `expected_link` | A file symbolic link exists and its normalized link target equals the expected value recorded in Known state. |
| `matching_unmanaged_link` | A symbolic link points to the desired source but no matching Known-state record exists. |
| `other_link` | A symbolic link exists but points elsewhere. |
| `other_entry` | A regular file, directory, junction, reparse point, or unsupported entry exists. |
| `unsafe_path` | A parent is missing, is not a directory, or is a symlink, junction, or reparse point. |

An entry classified as `matching_unmanaged_link` is not adopted.
It remains unmanaged and is reported as a conflict.

## Ownership and Removal

Known state records an expected link target, but it is not sufficient by itself to authorize deletion.
Loadout may remove a target only when current observation is `expected_link` for the resource's recorded link target.

Loadout MUST reject a target observed as `other_link`, `other_entry`, or `unsafe_path` without removing, replacing, following, or adopting that observed entry. This rejection does not protect substitutions after the final recheck, as specified in [External Filesystem Concurrency](#external-filesystem-concurrency).
It MUST NOT remove parent directories.

If a stale resource target is `missing`, Loadout may remove its Known state record without a filesystem mutation.
If its target is any other observation, removal is blocked by a conflict and Known state remains unchanged.

v0.2.0 does not provide a force option, unmanaged-target takeover, or user-invoked ownership-transfer operation.
The lifecycle may perform the internal managed-resource identity handoff defined by [Replace Ownership](#replace-ownership) only when Known and Actual state prove Loadout's existing ownership.

## Mutation Contract

Every filesystem mutation occurs only after a fresh executable plan and a successful preflight.
The executor MUST repeat the applicable source, containment, parent, target-kind, temporary and ownership checks immediately before each mutation step. A recheck failure prevents that step, not effects of earlier steps or actions; existing effects MUST be classified under [State and Recovery](state-and-recovery.md#mutation-result-classification), without replanning or automatic rollback.

### Create

Create requires a safe parent path, a verified source, and a `missing` target.
It creates the absolute symbolic link and then verifies `expected_link` against the planned source target.
No Known state is committed until that verification succeeds.

### Replace

Replace changes the source of a resource while retaining its target path.
It requires a verified source and an `expected_link` matching the old Known-state value.
The implementation MUST use a platform operation whose successful post-condition is the new expected link.
It MUST NOT first delete the old link and then attempt an unrelated create.

Replacement MUST use an atomic same-filesystem name replacement. If the platform cannot preserve the old link when the replacement operation itself fails, subject to the external-concurrency limit, preflight MUST block the action before new target mutation.

### Replace Ownership

Replace Ownership is an internal handoff between two Loadout-managed resource identities at one target.
It applies only when a stale old resource has a Known-state record, Actual observation proves the old resource's expected link, and the new resource source has validated.
It MUST NOT select an action to adopt, replace, or otherwise take over an observed unmanaged target, including a `matching_unmanaged_link`. The external-concurrency limitation also applies to this action.

If the old and new resolved link targets are equal, Loadout performs no filesystem mutation and allocates no replacement temporary path.
After a fresh no-follow verification of the old resource's ownership and the shared expected link, the state repository atomically removes the old Known resource identity, records the new identity, and marks the action `succeeded`.

If the resolved link targets differ, Replace Ownership MUST use the same replacement primitive and failure guarantees as Replace.
It MUST NOT delete the old managed link before installing the new expected link.
After the new expected link is verified, the state repository atomically replaces the old Known resource identity with the new identity and marks the action `succeeded`.

### Remove

Remove requires Known ownership and a fresh no-follow `expected_link` observation matching the Known-state value. It uses the rechecked parent directory and final component for a name-based removal without following the final link, then verifies that the recorded target is `missing` and its required path association holds.
The primitive MUST NOT remove the link referent or request parent-directory removal. A substituted final entry can nevertheless be removed after the recheck under the external-concurrency limit. If the required primitive and observations are unavailable, preflight MUST block without a new operation record or new target/Known mutation.

### Relocate

Relocate changes a resource target path.
It is planned as a create at the new missing target followed by a remove of the old expected link.
The create and its verification must complete before the old target is removed. Immediately before removal, the executor MUST repeat the applicable checks for both targets, source, containment and parents. A new link remaining together with the old link is partial relocation and is `uncertain`, with old Known unchanged and no automatic retry or rollback.
If the new target is not missing or the old target is not an expected link, the plan is blocked.

## Platform Requirements

### Intended Supported Scope

The v0.2.0 completion baseline is Linux on local ext4 (including ext4 within WSL2), macOS on local APFS, and Windows on local NTFS. Each combination requires create, remove, replace, relocate, both ownership handoffs, noop and forget-missing. This is intended support, not a claim that the current backend implements or has verified every capability. The [README](../../README.md#status) reports current implementation status.

Other operating systems and filesystems, network shares including SMB/NFS, WSL-mounted Windows volumes accessed through the Linux backend, and cross-filesystem replacement are outside the supported baseline. Accepted path spelling alone does not establish filesystem support. Unsupported capability requirements MUST be reported explicitly; an OS name alone is not evidence of the required filesystem guarantees.

Except for the existing Unix create transition below, each capability MUST have native filesystem, executor/application, compiled-binary and applicable recovery evidence on its baseline combination before enablement, as required by [Testing Strategy](../development/testing.md#platform-conformance). Windows additionally requires settled path/state semantics and native privilege/policy and target sharing/ACL evidence. No minimum OS release or architecture promise is introduced here: concrete API, OS, Rust target and filesystem requirements MUST be recorded before capability enablement. Removing a baseline platform or action requires an explicit scoped project-owner decision before changing the contract.

### Existing Unix Create Transition

The Unix create capability already enabled before S1 may remain enabled while the execution boundary and conformance evidence are completed. This is a narrow exception to the pre-enablement evidence gate, not a claim that the existing implementation satisfies every revised execution requirement. The current `cfg(unix)` backend does not restrict creation to Linux or identify the filesystem type. Consequently, creation can be attempted on macOS and other Unix/filesystem combinations for which native evidence has not been established. Only Linux/local ext4 creation has recorded native evidence in the current work record; capability availability alone MUST NOT be reported as verified support.

This exception preserves only the pre-S1 Unix create path. It does not authorize enabling currently rejected removal, replacement, source-changing handoff, relocation requiring removal, or Windows creation, and does not expand the supported baseline. The Unix execution-boundary work MUST bring create into the revised recheck and recorded-path observation contract. Before v0.2.0 completion, create still requires the full native evidence on every baseline combination, including macOS/APFS. The exception cannot be used to waive that completion requirement or infer support for excluded combinations.

### Supported Representation

On Unix, the implementation MUST create and inspect the symbolic-link entry without following its final target. Removal uses the immediate expected-link recheck and name-based deletion under the external-concurrency contract.
On Windows, it MUST create, replace, remove, and inspect a file symbolic link and MUST reject junctions and all unsupported reparse points.
An implementation MUST NOT fall back to copy, hard link, junction, a delayed-at-reboot operation, or another untracked filesystem operation.

### Operation Guarantees

The following guarantees apply to a target whose parent path and final entry have already passed the immediate no-follow safety recheck.

| Operation | Required guarantee |
| --- | --- |
| Create | Create only a file symbolic link at a target that is still `missing`. The implementation must not replace an entry that appeared after planning. |
| Replace | Record a unique Loadout-owned temporary sibling path, construct a new file symbolic link there, then use one target-name replacement operation. The temporary path is action-local and is never a declared resource target. It MUST NOT implement replacement as deleting the managed link and later creating a new one. Success requires both the new expected target link and absence of the temporary entry. Immediately before replacement, recheck both the expected old target and the expected recorded temporary under their shared safe parent, together with applicable source, containment and path-association predicates. If the platform cannot preserve the old expected link when the replacement operation itself fails, subject to external concurrency, it does not support `replace_link` and preflight MUST block the action. |
| Source-changing Replace Ownership | Apply every Replace guarantee. It is required only when the old and new resolved link targets differ. |
| Remove | Use a fresh no-follow expected-link recheck and name-based removal through the same retained parent and final component. Do not follow the link, remove its referent, or request parent-directory removal. An entry substituted after the recheck can be deleted; no atomic entry-identity binding is promised. |

The state repository allocates a unique temporary sibling path while it persists the operation record for every `replace_link` action and every `replace_ownership` action whose resolved link targets differ, before any mutation.
This allocation is an execution-local nonce, not a planner decision, resource identity, or ordering input.
The executor may use only the recorded path and MUST recheck that it is missing under the same safe parent immediately before creating the temporary link.

On Unix, replacement must use an atomic same-filesystem name replacement. A name-based primitive such as POSIX `unlinkat` deletes whichever entry currently has the supplied name; it is permitted only with the required immediate checks and observations under the external-concurrency contract. Checks and syscall MUST use the same retained parent context rather than re-resolving an absolute mutation pathname.

Cleanup is restricted to the exact recorded temporary path with a freshly observed expected link and safe parent/path association. It uses the same rechecked removal contract and concurrency limit; sibling scanning or general cleanup is forbidden. An observed unexpected temporary is preserved. Failure to complete or prove cleanup leaves uncertainty.
An open referent does not change the operation's ownership rule: removal and replacement are operations on the link entry, never on the referent.

On Windows, symbolic-link creation may be unavailable because of policy, privilege, developer-mode configuration, filesystem support, or access control.
Replacement and removal can also be rejected by ACLs or by an open handle whose sharing mode denies the required operation.
Loadout does not wait for the handle, schedule a later retry, or weaken the operation into delete-then-create.

### Capability, Permission, and Handle Failures

Preflight MUST block without a new operation record or new planned target mutation when it can determine that the platform cannot create a file symbolic link, cannot provide the required replacement guarantee for an action that requires replacement, or cannot provide the required rechecked no-follow removal and observation guarantees for an action that requires removal.
It must report the unsupported capability and affected action. Prior-operation recovery occurs before preflight and may already have performed its permitted temporary cleanup or state commits; these recovery effects are separate from execution of the new Plan, as defined by [Lifecycle](lifecycle.md#preflight).

Permission and handle availability are mutable filesystem facts and cannot be established conclusively by a separate access check.
An implementation MUST NOT treat a successful permission probe or an apparently unlocked target as authorization to skip the immediate safety recheck.
If a create, replacement, or removal attempt is denied by permission, sharing, a lock, a read-only filesystem, or another platform error, it follows the mutation-result protocol in [State and Recovery](state-and-recovery.md): it does not update Known state merely because the operation returned success or failure.

If the resulting post-condition cannot be proven, the action is `failed` only when its recorded precondition still holds exactly; otherwise it is `uncertain`.
In either case, no new Known-state fact is committed.

## Dry Run

A dry-run evaluates the same declaration, resolution, observation, and planning rules as apply.
It MUST NOT create links, target parents, temporary target files, operation records, state files, or locks.
