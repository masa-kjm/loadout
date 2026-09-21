# File-Copy Resource Specification

## Scope

This specification defines the v0.5.0 `copy` operation for a `file` resource.
It copies one regular file from a local store to one regular-file target below the current user's home directory.
It does not define directory copying, metadata synchronization, remote sources, import, adoption, or migration.

The source and target path syntax, physical containment requirements, no-follow parent checks, Windows path representation, and external filesystem concurrency limits in [File Links](file-link.md) apply to copies unless this specification states otherwise.
The same external-concurrency limit applies: checks reject observable substitutions but do not provide atomic entry identity between a final recheck and an operating-system call.

## Declaration and Resolution

A profile document using copy has schema version `2`.

```yaml
schema_version: 2
id: workstation
resources:
  git-config:
    type: file
    properties:
      kind: file
      operation: copy
      source:
        store: dotfiles
        path: git/config
      target: ~/.gitconfig
```

`kind` MUST be `file` and `operation` MUST be `copy`.
All fields are required and unknown fields are errors.
The source MUST be an existing regular file.
Resolution expands `target` only to an absolute target candidate and MUST NOT inspect its target entry or parent.
Actual inspection, preflight, and every executor mutation recheck prove target containment, parent safety, entry kind, and declared-path association as defined by [File Links](file-link.md).

Resolution for a copy computes `source_content_fingerprint` from the exact source bytes using SHA-256 and represents it as `sha256:<lowercase-hex>`.
It records no timestamp, mode, ACL, ownership, extended attribute, line-ending normalization, or other metadata in that fingerprint.
The copied target contains exactly those bytes; v0.5.0 neither preserves nor synchronizes source metadata.

The resolved copy definition is the JCS value:

```json
{
  "format": "loadout.resolved-file-copy.v1",
  "kind": "file",
  "operation": "copy",
  "source_path": "/home/example/dotfiles/git/config",
  "target_path": "/home/example/.gitconfig",
  "type": "file"
}
```

`source_content_fingerprint` is not a definition-hash input because it is current source state rather than declared resource identity.
It is a Resolved Desired input to planning and preflight.

## Actual and Ownership

Actual inspection first proves target containment and parent safety.
For a copy whose target parent is safe, Actual observation is one of `missing`, `expected_copy`, `other_regular_file`, `other_entry`, or `unsafe_path`.
`expected_copy` means a no-follow regular-file observation whose bytes hash to the Known record's `content_fingerprint`.
`other_regular_file` includes a file that hashes to the desired source content but has no matching Known record.
It is unmanaged and MUST NOT be adopted.

Known ownership for a copy consists of its fully qualified resource ID, resolved source and target paths, definition hash, and applied `content_fingerprint`.
Known state alone never authorizes replacement or removal.
For an action whose old effect is a copy, the executor requires both that record and a fresh `expected_copy` observation immediately before a destructive mutation.
The `link -> copy` handoff has the distinct old-effect predicate defined below.

## Planning

The planner receives typed copy Desired, Known, and Actual values and performs no source or target I/O.
The following rows apply to a copy at an unchanged target; `expected` means an `expected_copy` matching the Known fingerprint.

| Desired | Known | Actual | Required result |
| --- | --- | --- | --- |
| Present; no previous identity | Absent | Missing | `create_copy` |
| Present; no previous identity | Absent | Any entry | Blocked conflict; never adopt |
| Present; source fingerprint equals applied fingerprint | Present | Expected | `noop` |
| Present; source fingerprint differs from applied fingerprint | Present | Expected | `replace_copy` |
| Present | Present | Missing | `create_copy` |
| Present | Present | Other regular file, other entry, or unsafe path | Blocked conflict |
| Absent | Present | Expected | `remove_copy` |
| Absent | Present | Missing | `forget_missing` |
| Absent | Present | Any other observation | Blocked conflict |

A definition change that retains the target uses `replace_copy` only when Actual proves the old expected copy.
A target change uses `relocate_copy`: create and verify the new copy, then remove and verify the old expected copy as one contiguous action.
The general lifecycle rules define managed identity handoff and `link`/`copy` effect handoff; neither may adopt an unmanaged target.

## Mutation and Recovery

Before each mutation step, the executor rechecks source regular-file safety, source fingerprint, target containment, parent safety, declared-path association, target kind, and action-specific ownership.
It records `running` before the first mutation.

Create writes the verified source bytes to a unique action-local temporary regular file in the target parent, flushes it, hashes it, and verifies that its fingerprint equals the planned source fingerprint.
It then publishes the temporary only if the target is still missing.
Replace performs the same temporary preparation and atomically replaces only an expected owned target.
Neither operation may delete the final target before the new temporary has been completely written and verified.
If a platform cannot preserve the old expected target when publication fails, preflight MUST block replacement.

### Link-to-Copy Effect Handoff

`link -> copy` is a `replace_effect` action, not `replace_copy`.
Its precondition is a fresh no-follow `expected_link` observation matching the complete recorded file-link effect, not an `expected_copy` observation.
The executor writes the verified final source bytes to its unique recorded temporary regular file, flushes and hashes it, and proves that its fingerprint equals the recorded final copy fingerprint.
Immediately before one atomic same-filesystem target-name replacement, it rechecks the expected old link, exact temporary fingerprint and entry kind, parent safety, containment, and declared-path association.
Its postcondition is the expected final copy and absence of the recorded temporary.

The primitive MUST NOT delete the expected link before the final copy is ready and MUST NOT fall back to delete-then-create.
If the platform cannot preserve the old expected link when the replacement operation itself fails, subject to the external-concurrency limit, preflight MUST block `link -> copy` without a new operation record or target mutation.
The action records its complete old link and final copy predicates, temporary fingerprint, path, and recovery facts as defined by [State and Recovery](state-and-recovery.md#v050-state-schema).

The recorded temporary path and expected temporary fingerprint are part of every create or replacement action until publication is verified.
Immediately before publication, the executor rechecks both the exact temporary and final target.
It verifies the final target's bytes and that the temporary is missing before it commits the Known update and `succeeded` status atomically.

After an attempted mutation, the recorded post-condition authorizes `succeeded`; the recorded precondition authorizes `failed`; any other, unsafe, or unavailable observation is `uncertain`.
For a failed create followed by matching final content, the result is `uncertain`, not adoption.
Recovery may remove only the exact recorded temporary after a fresh no-follow proof that it is the expected temporary regular file.
It never scans sibling paths, deletes an unexpected temporary, retries an uncertain action, or rolls back a verified earlier action.

## Platform Requirements

The intended successful baseline is Linux/local ext4, macOS/local APFS, and Windows/local NTFS.
Each enabled platform/action combination requires native filesystem, executor, compiled-binary, and applicable recovery evidence.
Windows coverage includes sharing or ACL denial when the runner can establish it and proves no delete-then-create fallback or premature Known update.
If a required publication or no-follow observation guarantee is unavailable, preflight MUST reject the action without a new operation record or target mutation.
