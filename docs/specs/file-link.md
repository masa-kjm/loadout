# File-Link Resource Specification

## Scope

This specification defines the only v0.2.0 resource implementation: a regular file from a local store materialized as a file symbolic link below the current user's home directory.
It defines declaration syntax, containment, ownership, observation, mutation preconditions, and platform requirements.

Copy operations, directory resources, hard links, junctions, and remote stores are outside v0.2.0.

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

Loadout MUST NOT remove, replace, follow, or adopt an `other_link`, `other_entry`, or `unsafe_path` target.
It MUST NOT remove parent directories.

If a stale resource target is `missing`, Loadout may remove its Known state record without a filesystem mutation.
If its target is any other observation, removal is blocked by a conflict and Known state remains unchanged.

v0.2.0 does not provide a force option, unmanaged-target takeover, or user-invoked ownership-transfer operation.
The lifecycle may perform the internal managed-resource identity handoff defined by [Replace Ownership](#replace-ownership) only when Known and Actual state prove Loadout's existing ownership.

## Mutation Contract

Every filesystem mutation occurs only after a fresh executable plan and a successful preflight.
The executor MUST repeat the source, containment, parent, target-kind, and ownership checks immediately before the mutation.

### Create

Create requires a safe parent path, a verified source, and a `missing` target.
It creates the absolute symbolic link and then verifies `expected_link` against the planned source target.
No Known state is committed until that verification succeeds.

### Replace

Replace changes the source of a resource while retaining its target path.
It requires a verified source and an `expected_link` matching the old Known-state value.
The implementation MUST use a platform operation whose successful post-condition is the new expected link.
It MUST NOT first delete the old link and then attempt an unrelated create.

If the platform cannot perform this replacement while preserving the old link when the replacement fails, preflight MUST block the action before the target is changed.

### Replace Ownership

Replace Ownership is an internal handoff between two Loadout-managed resource identities at one target.
It applies only when a stale old resource has a Known-state record, Actual observation proves the old resource's expected link, and the new resource source has validated.
It MUST NOT adopt, replace, or otherwise take over an unmanaged target, including a `matching_unmanaged_link`.

If the old and new resolved link targets are equal, Loadout performs no filesystem mutation and allocates no replacement temporary path.
After a fresh no-follow verification of the old resource's ownership and the shared expected link, the state repository atomically removes the old Known resource identity, records the new identity, and marks the action `succeeded`.

If the resolved link targets differ, Replace Ownership MUST use the same replacement primitive and failure guarantees as Replace.
It MUST NOT delete the old managed link before installing the new expected link.
After the new expected link is verified, the state repository atomically replaces the old Known resource identity with the new identity and marks the action `succeeded`.

### Remove

Remove requires `expected_link` matching the Known-state value.
It removes only the final symbolic-link entry and then verifies that the target is `missing`.
No parent directory is removed.

### Relocate

Relocate changes a resource target path.
It is planned as a create at the new missing target followed by a remove of the old expected link.
The create and its verification must complete before the old target is removed.
If the new target is not missing or the old target is not an expected link, the plan is blocked.

## Platform Requirements

### Supported Representation

On Unix, the implementation MUST create, replace, remove, and inspect the symbolic-link entry without following its final target.
On Windows, it MUST create, replace, remove, and inspect a file symbolic link and MUST reject junctions and all unsupported reparse points.
An implementation MUST NOT fall back to copy, hard link, junction, a delayed-at-reboot operation, or another untracked filesystem operation.

### Operation Guarantees

The following guarantees apply to a target whose parent path and final entry have already passed the immediate no-follow safety recheck.

| Operation | Required guarantee |
| --- | --- |
| Create | Create only a file symbolic link at a target that is still `missing`. The implementation must not replace an entry that appeared after planning. |
| Replace | Record a unique Loadout-owned temporary sibling path, construct a new file symbolic link there, then use one target-name replacement operation. The temporary path is action-local and is never a declared resource target. It MUST NOT implement replacement as deleting the managed link and later creating a new one. Success requires both the new expected target link and absence of the temporary entry. If the platform cannot preserve the old expected link when that replacement operation fails, it does not support `replace_link` and preflight MUST block the action. |
| Source-changing Replace Ownership | Apply every Replace guarantee. It is required only when the old and new resolved link targets differ. |
| Remove | Remove only the final expected file-symbolic-link entry. It must not follow the link, remove its referent, or remove a parent directory. |

The state repository allocates a unique temporary sibling path while it persists the operation record for every `replace_link` action and every `replace_ownership` action whose resolved link targets differ, before any mutation.
This allocation is an execution-local nonce, not a planner decision, resource identity, or ordering input.
The executor may use only the recorded path and MUST recheck that it is missing under the same safe parent immediately before creating the temporary link.

On Unix, replacement must use an atomic same-filesystem name replacement, and removal must use a link-entry removal operation.
An open referent does not change the operation's ownership rule: removal and replacement are operations on the link entry, never on the referent.

On Windows, symbolic-link creation may be unavailable because of policy, privilege, developer-mode configuration, filesystem support, or access control.
Replacement and removal can also be rejected by ACLs or by an open handle whose sharing mode denies the required operation.
Loadout does not wait for the handle, schedule a later retry, or weaken the operation into delete-then-create.

### Capability, Permission, and Handle Failures

Preflight MUST block without target mutation when it can determine that the platform cannot create a file symbolic link or cannot provide the required replacement guarantee for an action that requires replacement.
It must report the unsupported capability and affected action.

Permission and handle availability are mutable filesystem facts and cannot be established conclusively by a separate access check.
An implementation MUST NOT treat a successful permission probe or an apparently unlocked target as authorization to skip the immediate safety recheck.
If a create, replacement, or removal attempt is denied by permission, sharing, a lock, a read-only filesystem, or another platform error, it follows the mutation-result protocol in [State and Recovery](state-and-recovery.md): it does not update Known state merely because the operation returned success or failure.

If the resulting post-condition cannot be proven, the action is `failed` only when its recorded precondition still holds exactly; otherwise it is `uncertain`.
In either case, no new Known-state fact is committed.

## Dry Run

A dry-run evaluates the same declaration, resolution, observation, and planning rules as apply.
It MUST NOT create links, target parents, temporary target files, operation records, state files, or locks.
