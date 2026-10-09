# Directory-Link Resource Specification

## Scope

This specification defines the `link` operation for a local-store real directory materialized as a directory symbolic link below the current user's home directory.
It defines declaration and resolution, typed Desired, Known, and Actual values, ownership, planning transitions, and mutation aftermath.

Directory copy, tree synchronization, manifests, descendant ownership, directory traversal below the final source, recursive deletion, metadata or ACL synchronization, junction creation, junction compatibility, and effect handoff between `directory_link` and either `file_link` or `file_copy` are outside the current scope.

The target path, parent safety, declared-path association, Windows path representation, and external filesystem concurrency contract in [File Links](file-link.md) apply unless this specification states otherwise.
The source and target entry are always observed without following their final symbolic link.

## Declaration and Resolution

A directory link retains `type: file` and declares `kind: directory` with `operation: link`.

```yaml
type: file
properties:
  kind: directory
  operation: link
  source:
    store: dotfiles
    path: nvim
  target: ~/.config/nvim
```

All shown fields are REQUIRED and unknown fields are errors.
The accepted `(kind, operation)` pairs are a closed set: `(file, link)`, `(file, copy)`, and `(directory, link)`.
`(directory, copy)` is invalid.

`source.store` and `source.path` use the same syntax and local-store binding rules as [File Links](file-link.md#source-resolution).
Every existing source-path component from the physical store root through the source parent MUST be a real directory and MUST NOT be a symbolic link, junction, or other reparse point.
The final source MUST be an existing real directory and MUST NOT be a symbolic link, junction, or other reparse point.
Resolution does not read, traverse, fingerprint, or otherwise inspect children of that final directory.

Resolution expands the target only to an absolute candidate and does not inspect a target entry or parent.
Actual inspection, preflight, and every mutation recheck prove target containment, safe existing parents, and declared-path association under the File Links contract.
Loadout does not create target parents.

The resolved directory-link definition is the following JCS value, whose UTF-8 SHA-256 is its `definition_hash`:

```json
{
  "format": "loadout.resolved-directory-link.v1",
  "kind": "directory",
  "operation": "link",
  "source_path": "/home/example/dotfiles/nvim",
  "target_path": "/home/example/.config/nvim",
  "type": "file"
}
```

The value has no resource ID, store ID, raw declaration syntax, source-directory contents, metadata, or platform schema version.
`desired_hash` includes this definition with its fully qualified resource ID using the current canonical desired-set representation in [State and Recovery](state-and-recovery.md#current-canonical-hashes).

## Typed Actual and Known Effects

`DirectoryLink` is a distinct typed effect, `directory_link`.
Its Known ownership evidence is the fully qualified resource ID, `definition_hash`, resolved source path, resolved target path, and exact normalized absolute link target recorded after verification.
Known state alone never authorizes removal or replacement.

After target containment and parent safety have been proved, Actual is one of:

| Observation | Meaning |
| --- | --- |
| `missing` | No entry exists at the target. |
| `expected_directory_link` | A directory symbolic link exists and its exact normalized link target equals the recorded `directory_link` target. |
| `matching_unmanaged_directory_link` | A directory symbolic link points to the desired source but there is no matching Known `directory_link` effect. |
| `other_link` | A file symbolic link, a directory symbolic link to another target, or a link whose directory kind cannot be established. |
| `other_entry` | A regular file, regular directory, junction, other reparse point, or unsupported entry exists. |
| `unsafe_path` | A parent is missing, non-directory, a symbolic link, junction, or other reparse point, or required containment/path association cannot be proved. |

On Unix, `expected_directory_link` is proved by the typed Known effect and a no-follow read of the exact link target.
It does not require the referent to exist or a directory/file kind from the symbolic-link entry.
On Windows, the filesystem implementation alone classifies a directory symbolic link, a file symbolic link, a junction, and another reparse point; a junction or another reparse point is never `expected_directory_link`.

Every `matching_unmanaged_directory_link`, `other_link`, `other_entry`, and `unsafe_path` is a conflict.
Loadout MUST NOT adopt, replace, remove, follow, or reinterpret it as a managed directory link.

## Planning Transitions

The planner is pure and receives only typed resolved DirectoryLink Desired, Known, and Actual values.
`expected` below means `expected_directory_link` for the exact recorded Known effect.
`unexpected` means every non-missing, non-expected observation above.

| Desired | Known | Actual | Required result |
| --- | --- | --- | --- |
| Present; no previous identity | Absent | Missing | `create_directory_link` |
| Present; no previous identity | Absent | Any entry | Blocked conflict; never adopt |
| Present; unchanged definition | Present | Expected | `noop` |
| Present; unchanged definition | Present | Missing | `create_directory_link` |
| Present; unchanged definition | Present | Unexpected | Blocked conflict |
| Present; source changed, same target | Present | Expected | `replace_directory_link` |
| Present; source changed, same target | Present | Missing | `create_directory_link` |
| Present; source changed, same target | Present | Unexpected | Blocked conflict |
| Present; target changed | Present | Old target expected and new target missing | `relocate_directory_link` |
| Present; target changed | Present | Any other old or new target observation | Blocked conflict |
| Absent | Present | Expected | `remove_directory_link` |
| Absent | Present | Missing | `forget_missing` |
| Absent | Present | Unexpected | Blocked conflict |
| New DirectoryLink identity claims a target held by a stale DirectoryLink identity | Old identity present | Old target expected | `replace_ownership` |
| New DirectoryLink identity claims a target held by a stale DirectoryLink identity | Old identity present | Old target missing | `create_directory_link` with the stale identity recorded as superseded |
| New DirectoryLink identity claims a target held by stale `file_link` or `file_copy` identity, or conversely | Any | Any | Blocked conflict |
| Multiple Desired identities claim one normalized target | Any | Any | Blocked conflict |

An unchanged directory link does not inspect source descendants, so source-child changes produce no action.
For a managed same-target identity handoff, equal exact link targets are state-only after a final fresh expected-old-effect verification.
Different link targets use `replace_ownership` with the directory-link replacement contract.
The old Known identity is removed and the new `directory_link` identity is recorded only with verified success.
When the shared target is missing, the planner instead selects `create_directory_link` and records the stale identity as superseded.
Only verified create success atomically removes that old Known identity and records the new one.
Failed or uncertain creation, including a failed create followed by an observed matching directory link, retains the stale Known identity and never adopts the observed link.

DirectoryLink never uses `replace_effect`.
The file-link/file-copy effect-handoff contract does not apply to it.

## Mutation and Failure Aftermath

Every action is selected by the fresh Plan, passes preflight, is recorded as `running` before its first mutation, and rechecks applicable containment, parent safety, target kind, declared-path association, and action-specific ownership immediately before each mutation.
Create, replace, relocate, and source-changing identity handoff additionally recheck final-source real-directory safety.
The executor does not replan after a failed recheck.
The [State and Recovery](state-and-recovery.md#mutation-result-classification) specification owns durable operation records, status classification, Known commits, and recovery.

`create_directory_link` requires a missing target and creates only a directory symbolic link with the absolute verified source as its link target.
It MUST NOT overwrite an entry that appears after planning.
After a failed create attempt, even an observed matching directory link is `uncertain` and is not adopted; a proven missing target is `failed` and every other, unsafe, or unavailable observation is `uncertain`.

`replace_directory_link` and source-changing `replace_ownership` require an expected old directory link and a verified final source directory.
They create a unique action-local directory-link temporary sibling and use atomic same-filesystem name replacement.
They MUST NOT delete the old target and later create a new link.
If the platform cannot preserve the old expected link when replacement itself fails, preflight blocks without a new operation record or planned target mutation.
Their temporary predicates, durable records, rechecks, cleanup, and recovery classification are defined by [Directory-Link Operation Records](state-and-recovery.md#directory-link-operation-records).

`remove_directory_link` requires both the typed Known effect and a fresh expected-directory-link observation.
It removes only the final link entry by name and then proves the recorded target is missing with its required path association.
It MUST NOT remove the link referent, request parent removal, traverse a directory, or recursively delete.
It does not require the former source to exist or remain a directory.

`relocate_directory_link` creates and verifies the new link at a missing target before removing and verifying the old expected link as one contiguous action.
Both target paths, source safety, containment, and parents are rechecked immediately before old-target removal.
A partial relocation, including both links remaining, is `uncertain`; it is neither rolled back nor automatically retried.

`forget_missing` removes only the stale Known effect when its recorded target is already proved missing; it performs no target mutation.
The external filesystem concurrency contract applies to all observations and mutations.
It can make a later substituted entry indistinguishable from normal removal, so a complete recorded missing post-condition may still classify as `succeeded` without proving which entry was removed.

## Platform Requirements

The intended successful baseline is Linux/local ext4, macOS/local APFS, and Windows/local NTFS.
Each enabled platform/action combination requires native filesystem, executor, compiled-binary, and applicable recovery evidence before enablement.

Unix creates and observes a symbolic-link entry without following the final target.
Windows creates only a directory symbolic link and has no junction, copy, hard-link, delayed-operation, or fallback implementation.
If directory-link creation, safe no-follow removal/observation, or the required replacement guarantee is unavailable, preflight MUST reject the affected action without a new operation record or target mutation.
