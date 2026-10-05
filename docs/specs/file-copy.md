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
A stale managed copy identity at the same target may transfer to one new copy identity through `replace_ownership` only when Actual proves the old expected copy.
When the final resolved source fingerprint equals the old applied fingerprint, that handoff is state-only.
When it differs, it uses the staged copy replacement contract below and replaces the old Known identity with the new one only after the final copy is verified.
If the shared target is missing, the identity handoff uses `create_copy`; its verified success supersedes the stale old Known identity, while an unsuccessful create leaves that old Known identity unchanged.

## Mutation and Recovery

Before each mutation step, the executor rechecks source regular-file safety, source fingerprint, target containment, parent safety, declared-path association, target kind, and action-specific ownership.
It records `running` before the first mutation.

`create_copy`, including the new-target step of `relocate_copy`, uses `create-no-replace`: it creates the final regular-file copy only when the final target is still missing and MUST NOT overwrite an entry that appeared after planning.
An implementation may use either an exclusive direct final-file create or a temporary followed by atomic no-replace publication, provided that it satisfies every required recheck, byte, flush, and post-mutation observation.
A direct create may leave an incomplete final target after a failed write, flush, close, or verification.
That entry is not Desired merely because its pathname is the final target: unless the recorded postcondition proves the exact planned bytes, it is `uncertain` and Loadout MUST NOT delete, overwrite, adopt, or retry it automatically.

`replace_copy` prepares, flushes, hashes, and verifies a unique action-local temporary regular file before it removes a freshly proved expected owned old copy.
It atomically persists `temporary_staged` after that verification and before any old-copy removal.
Immediately before removal, it repeats the old-copy ownership, target containment, parent safety, and declared-path association checks.
It then deletes the old target and publishes the verified temporary to the now-missing final name with no-replace semantics.
It MUST NOT delete the final target before the temporary has been completely written and verified, overwrite an entry that appeared at the final name, use a backup-and-restore workflow, schedule a delayed operation, or retry the old action automatically.
`replace_copy` deliberately does not preserve the old target pathname when removal or publication fails and does not restore old content.
Its failure aftermath is classified from the recorded old-copy precondition, new-copy postcondition, and the safe final-target observation defined below.

A copy `replace_ownership` action with differing old and final fingerprints uses this exact mutation sequence and recovery contract.
Its action record additionally identifies the old and new resource identities, and its verified success atomically removes the old Known copy while recording the final copy under the new identity.

### Publication Primitives and Capability Boundary

Every copy mutation addresses the final name, and any action-local temporary name, through the already rechecked target parent; it MUST NOT re-resolve an absolute target pathname for a mutation.
`create-no-replace` does not fix an implementation technique: an exclusive direct final-file create, temporary plus atomic no-replace publication, or a platform-specific implementation with equivalent native evidence is permitted.
The following platform entries describe required properties rather than an exclusive implementation list.

| Platform and action | Permitted primitive | Required capability outcome |
| --- | --- | --- |
| Linux/local ext4 create | A retained-parent `create-no-replace` implementation | It must reject an already existing final name without changing it. Unsupported required syscall or filesystem behavior is a preflight rejection. |
| macOS/local APFS create | A retained-parent `create-no-replace` implementation after the required volume capability check | It must reject an already existing final name without changing it. Unsupported volume capability is a preflight rejection. |
| Windows/local NTFS create | A no-replace implementation after the reparse-point and declared-path rechecks | It must reject an already existing final name without changing it. Any behavior that can replace an existing final name is unsupported and blocks preflight. |
| Copy replacement and copy identity handoff with changed bytes | Verified temporary staging, rechecked owned-old removal, then retained-parent no-replace publication | It must reject an entry that appears at the final name and permit the copy recovery classification defined below. It need not preserve the old copy at its pathname on failure. |
| `link -> copy` handoff | Verified copy temporary staging, rechecked owned-link removal, then retained-parent no-replace publication | It follows the copy handoff lifecycle defined below and does not require old-link preservation after removal. |
| `copy -> link` handoff | The primitive defined by [File Links](file-link.md) | It follows the effect-handoff lifecycle and does not require old-copy preservation after removal. |

No copy action may use a cross-directory move, backup-name workflow, delayed operation, or fallback primitive.
The sequential remove-then-no-replace-publication sequence is permitted only for `replace_copy`, copy `replace_ownership` with changed bytes, and `link -> copy` after their required staging and immediate old-effect recheck.
Native conformance must prove each selected platform/action implementation's success, existing-target collision preservation where `create-no-replace` is used, error aftermath, post-mutation classification, and applicable recovery before that capability is enabled.

### Link-to-Copy Effect Handoff

`link -> copy` is a `replace_effect` action, not `replace_copy`.
Its precondition is a fresh no-follow `expected_link` observation matching the complete recorded file-link effect, not an `expected_copy` observation.
The executor writes the verified final source bytes to its unique recorded temporary regular file, flushes and hashes it, and proves that its fingerprint equals the recorded final copy fingerprint.
It atomically persists `temporary_staged` before it may remove the old link.
Immediately before it removes the old link, it rechecks the expected old link, exact temporary fingerprint and entry kind, parent safety, containment, and declared-path association.
It then removes the old link and publishes the temporary to the missing final name with no-replace semantics.
Its postcondition is the expected final copy and absence of the recorded temporary.

The primitive MUST NOT delete the expected link before the final copy is ready or overwrite an entry that appears after removal.
It does not restore the old link if final publication fails.
The action records its complete old link and final copy predicates, temporary fingerprint, path, and publication facts as defined by [State and Recovery](state-and-recovery.md#v050-state-schema).

For staged publication, the recorded temporary path and expected temporary fingerprint are part of the action until publication is verified.
Immediately before staged publication, the executor rechecks both the exact temporary and final target.
For direct create, it rechecks the final target immediately before creation and after every attempted mutation boundary required to establish the postcondition.
It verifies the final target's bytes and, for a staged action, that the temporary is missing before it commits the Known update and `succeeded` status atomically.

After an attempted `create-no-replace`, the recorded post-condition authorizes `succeeded`; the recorded missing precondition authorizes `failed`; any other, unsafe, or unavailable observation is `uncertain`.
For a failed create followed by matching final content, the result is `uncertain`, not adoption.
For `replace_copy`, copy `replace_ownership` with changed bytes, and `link -> copy`, a recorded final-copy postcondition with `publication_attempted` authorizes `succeeded`; the exact recorded old-effect precondition authorizes `failed` after any eligible-temporary cleanup; a missing final target also authorizes `failed` after eligible-temporary cleanup; and a safely observed different entry authorizes `failed` with a conflict.
An unsafe or unavailable final observation is `uncertain`.
Recovery may remove only an eligible recorded temporary after a fresh no-follow proof of its recorded pathname, expected regular-file kind, expected fingerprint, and safe parent association.
When eligible-temporary cleanup is required before a `failed` result, it is complete only when the recorded temporary pathname is freshly proven missing; the removal call's return value is not sufficient.
Denied, unprovable, or non-missing cleanup aftermath is `uncertain` and retains the active operation as a global barrier until an operator corrects the exact recorded artifact and recovery can reclassify it.
This practical cleanup boundary does not identify a hostile substitution at the same random temporary pathname with the same bytes.
It never scans sibling paths, deletes an unexpected temporary, retries an uncertain action, or rolls back a verified earlier action.

## Platform Requirements

The intended successful baseline is Linux/local ext4, macOS/local APFS, and Windows/local NTFS.
Each enabled platform/action combination requires native filesystem, executor, compiled-binary, and applicable recovery evidence.
Windows coverage includes sharing or ACL denial when the runner can establish it and proves no overwrite, premature Known update, backup, or automatic restoration.
If a required publication or no-follow observation guarantee is unavailable, preflight MUST reject the action without a new operation record or target mutation.
