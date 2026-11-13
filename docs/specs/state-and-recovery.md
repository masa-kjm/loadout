# State and Recovery Specification

## Scope

This specification defines v0.8.0 durable state, operation records, exclusive locking, atomic commits, applied lifecycle topology, and recovery after interruption for file-link, file-copy, and DirectoryLink resource effects.

Normal commands use only the current state schema. The explicit schema-4-to-5 authoring route is defined by [Migration](migration.md).

## Read by Task

Use these routes to locate the owning sections. For a resource mutation, also read its [resource specification](README.md#read-by-concern) and the [Lifecycle](lifecycle.md) contract.

| Task | Start here | Continue with |
| --- | --- | --- |
| State decoding or hash changes | [Current State Schema](#current-state-schema), [Current Canonical Hashes](#current-canonical-hashes) | [Applied Lifecycle Topology and Operations](#applied-lifecycle-topology-and-operations) for graph or topology fields. |
| Action-record shape or durable progress | [Operation Record](#operation-record), then [File Effect Action Records](#file-effect-action-records) or [Directory-Link Operation Records](#directory-link-operation-records) | [Applied Lifecycle Topology and Operations](#applied-lifecycle-topology-and-operations) for scheduled actions and topology facts. |
| State locking or publication | [Locking](#locking), [Commit Protocol](#commit-protocol) | [Mutation Result Classification](#mutation-result-classification) when a target mutation may have occurred. |
| Interrupted action or recovery | [Failure and Recovery](#failure-and-recovery), [Mutation Result Classification](#mutation-result-classification) | [Operation Record](#operation-record), the applicable [file](#file-effect-action-records) or [directory-link](#directory-link-operation-records) record, and [Commit Protocol](#commit-protocol). |

## State Files

The state repository owns these files in the platform state directory defined by [Configuration](configuration.md):

```text
state.json
state.lock
```

`state.json` is UTF-8 JSON.
If it does not exist, Loadout starts with an empty current Known state.
The state directory and lock file may be created by a non-dry-run apply.

If `state.json` is unreadable, invalid JSON, has an unsupported schema version, or violates an invariant, Loadout MUST abort before it inspects a managed target or creates a mutation.
It MUST NOT repair, migrate, or overwrite the state automatically.

## Current State Schema

Normal lifecycle and inspection commands use this schema for every state document they read.
`state.json` has `schema_version: 5`; schema version `4`, every earlier version, and every unknown future version are invalid.
The state repository rejects an invalid state version before target inspection, operation recovery, planning, state rewrite, or any target mutation.
It does not decode the older document for conversion, recalculate its hashes, reinterpret an active operation, or write a replacement state file.
Only the explicit migration operation in [Migration](migration.md) may parse a supported schema-4 state document.

An absent state file is an empty Known state.
An existing schema-4 state file is not an empty state: it is an error that preserves the file and prevents a new plan.
Its recorded effects and active operation are not ownership evidence and MUST NOT be observed, adopted, recovered, removed, replaced, or otherwise acted upon.

The logical Known-state structure is:

```json
{
  "schema_version": 5,
  "resources": {
    "base/nvim-config": {
      "definition_hash": "sha256:...",
      "effect": {
        "kind": "directory_link",
        "source_path": "/home/example/dotfiles/nvim",
        "target_path": "/home/example/.config/nvim",
        "link_target": "/home/example/dotfiles/nvim"
      }
    }
  },
  "applied_lifecycle_topology": { "current": [], "historical": [] },
  "active_operation": null
}
```

`schema_version`, `resources`, `applied_lifecycle_topology`, and `active_operation` are REQUIRED.
Unknown fields are errors at every object level.
Each resource member contains exactly `definition_hash` and `effect`.
`effect.kind` is exactly `file_link`, `file_copy`, or `directory_link`.
A `file_link` records `source_path`, `target_path`, and `link_target`; a `file_copy` records `source_path`, `target_path`, and `content_fingerprint`; and a `directory_link` records `source_path`, `target_path`, and `link_target`.
Each effect rejects fields that do not belong to its kind.
Resource IDs and normalized target paths are unique, all paths are absolute platform-normalized paths without home shorthand or relative components, and every definition hash and fingerprint has the required canonical form.
On Windows, every stored path and operation path uses the normal DOS or ordinary UNC representation in [Windows Path Representation](file-link.md#windows-path-representation).
A path alias or mismatched recorded hash is not repaired or reinterpreted to establish ownership.
Known state records only verified post-conditions; a later destructive action still requires current Actual ownership proof.

`active_operation` is either `null` or a schema-5 action-graph operation record. Its resource-specific preconditions and postconditions retain the applicable [Directory-Link Operation Records](#directory-link-operation-records) safety shapes.
An unrecognized action kind, predicate, effect, field, status, or invariant is invalid state and is rejected before recovery or target inspection.

## Canonical Hashes

### Current Canonical Hashes

`definition_hash` and `desired_hash` are SHA-256 hashes of UTF-8 JSON Canonicalization Scheme (JCS; RFC 8785) values and use `sha256:<lowercase-hex>` representation.
They are not derived from source YAML or JSON serialization. Resolved paths receive no additional case, separator, or Unicode normalization while hashing.
A copy's source bytes are not a definition-hash input; its verified Known effect records the applied content fingerprint.
The exact per-effect definition value is the value owned by [File Links](file-link.md), [File Copies](file-copy.md), or [Directory Links](directory-link.md#declaration-and-resolution).
The DirectoryLink definition is `loadout.resolved-directory-link.v1`; it includes `format`, `type`, `kind`, `operation`, `source_path`, and `target_path`, and excludes the resource ID, raw YAML, store ID, source-directory contents, metadata, and schema version.

The current desired hash is exactly the hash of this JCS value; orchestration facts are excluded:

```json
{
  "format": "loadout.resolved-desired.v5",
  "resources": [
    {
      "definition": {
        "format": "loadout.resolved-directory-link.v1",
        "kind": "directory",
        "operation": "link",
        "source_path": "/home/example/dotfiles/nvim",
        "target_path": "/home/example/.config/nvim",
        "type": "file"
      },
      "resource_id": "base/nvim-config"
    }
  ]
}
```

`resources` sorts lexicographically by fully qualified `resource_id`.
The resource ID is an input only to `desired_hash`; it is not an input to `definition_hash`.
Schema versions, YAML mapping order, profile discovery/include order, raw declaration syntax, store IDs, source file content, source-directory contents, and metadata are not hash inputs.
Schema-4 hashes are not aliases for schema-5 hashes and MUST NOT be accepted, recalculated, or compared to establish ownership by a normal command.

## Operation Record

An active operation records the selected work and the facts needed for recovery without rereading its original profile.
It has a newly generated opaque `id`, the [Canonical Hashes](#canonical-hashes) `desired_hash` of the original Resolved Desired set, and an `actions` object keyed by opaque action IDs.
Each action records typed resolved preconditions and postconditions, its kind, resource ID, and status; the operation also records the graph facts in [Applied Lifecycle Topology and Operations](#applied-lifecycle-topology-and-operations).
Unknown fields or invalid combinations are invalid state.

Each action status is one of:

| Status | Meaning |
| --- | --- |
| `pending` | The action has not started. |
| `running` | The start was committed, but completion is not known. |
| `succeeded` | The required post-condition was verified and the Known and authorized topology updates were committed. |
| `failed` | A definite failure was classified; prior Known facts remain unless an action-specific contract says otherwise. |
| `skipped` | The action was not attempted because a recorded dependency or global abort prevented it. |
| `uncertain` | Recorded observations cannot prove an authorized result. |

`pending`, `running`, and `uncertain` are not successful states.

For `replace_link`, `remove_link`, and link `replace_ownership`, the precondition records the old expected link target.
For `replace_link`, and for link `replace_ownership` whose resolved link targets differ, the record also contains its unique action-local temporary sibling path and expected link target.
The temporary is not a declared resource target and is used only for the recorded replacement or safe cleanup.
A relocate action records both old and new targets and their required observations.
`replace_ownership` records `old_resource_id`, `new_resource_id`, complete old and final effects, and the applicable expected effect predicates; equal link targets have no temporary.

### File Effect Action Records

Every copy action records its effect kind, resolved source and target, planned source fingerprint, precondition, and postcondition.
A staged copy publication, every `replace_copy`, every copy `replace_ownership` with changed bytes, and every `link -> copy` effect handoff also records its action-local temporary path and fingerprint until publication completes.
For such an action, `temporary_staged` becomes true only after the temporary has been verified at its recorded path with its expected regular-file kind, fingerprint, and safe parent association.
The state repository MUST atomically persist `temporary_staged == true` before the executor performs any destructive old-effect mutation.
An eligible staged copy temporary requires that durable fact and a fresh no-follow observation of the recorded pathname, expected regular-file kind, expected fingerprint, and safe parent association.
An action MUST NOT clean up a temporary that lacks `temporary_staged == true`, even if its current pathname, kind, and fingerprint match the record.
Every copy action that creates or publishes a final copy, and every `copy -> link` effect handoff, records `publication_attempted`; it becomes true immediately before that final publication attempt.
Neither fact authorizes a Known update without the required postcondition observation.
For an action with a temporary, that temporary is part of the post-condition and must be missing before `succeeded` is committed.
A direct `create-no-replace` action has no temporary and records the final missing precondition and exact final-copy postcondition needed to classify an incomplete or otherwise unproved final entry as `uncertain`.
`replace_effect` and `replace_ownership` record both old and final effects and require the old effect's ownership predicate before any final-effect publication.

Every action contains `kind`, `resource_id`, typed `precondition`, typed `postcondition`, and `status`.
Copy publication actions additionally contain the boolean `publication_attempted`; staged copy actions additionally contain the boolean `temporary_staged`.
A `create_copy` action selected for a missing-target copy identity handoff additionally contains `superseded_resource_id`; no other `create_copy` action contains that field.
The complete `old_effect` and `final_effect` objects provide the corresponding Known-state update values; predicates repeat the exact target ownership facts needed for recovery.
`create_copy`, `replace_copy`, `relocate_copy`, `remove_copy`, `replace_effect`, and copy `replace_ownership` use the `file_copy` predicate with its fingerprint.
`create_link`, `replace_link`, `relocate_link`, `remove_link`, `replace_effect`, and link `replace_ownership` use the `file_link` predicate with its exact link target.
Staged copy publication, `replace_copy`, copy `replace_ownership` with changed bytes, and `link -> copy` have `temporary`; direct `create-no-replace`, `copy -> link`, remove, forget, and state-only identity actions do not.
Relocation additionally records distinct old and new typed effects.

| Action kind | Typed precondition | Typed postcondition | Known update |
| --- | --- | --- | --- |
| `create_link` | target missing | expected final link | record final link |
| `create_copy` | target missing | expected final copy, and temporary missing when staged | record final copy; replace a recorded superseded stale identity when present |
| `replace_link` | expected old link | expected final link and temporary missing | replace link effect |
| `replace_copy` | expected old copy | expected final copy and temporary missing | replace copy effect |
| `replace_effect` | expected complete old effect | expected complete final effect and temporary missing when final copy is staged | replace old effect with final effect |
| `relocate_link` / `relocate_copy` | expected old effect and missing new target | expected final new effect and missing old target | replace recorded target/effect |
| `remove_link` / `remove_copy` | expected old effect | target missing | remove effect |
| `forget_missing` | target missing | target missing | remove stale effect |
| `replace_ownership` | expected old effect | expected final effect | replace old identity with new identity |

`replace_ownership` records `old_resource_id`, `new_resource_id`, and complete old/final effects.
When its effects share the same target and prove the same expected link or copy fingerprint, it has no temporary and its `running` transition precedes the final ownership verification.
When both effects are copies at the same target but their fingerprints differ, it records the staged final-copy temporary, `temporary_staged`, and `publication_attempted`, then uses the sequential copy-replacement predicates and aftermath.
When both effects are links with different resolved targets, it uses the final link replacement temporary and the same strong recovery predicates as `replace_link`.
Mixed link/copy effects use `replace_effect`, not `replace_ownership`.
A `create_copy` selected for a missing-target identity handoff records `superseded_resource_id`; its successful Known update removes that stale record and records the final copy under the new resource identity.

### Directory-Link Operation Records

The [Current State Schema](#current-state-schema) defines `directory_link` as a third typed Known effect and its serialized schema version and exact JSON shape.
Its definition and ownership fields are the resolved source path, resolved target path, exact normalized link target, and definition hash in [Directory Links](directory-link.md#typed-actual-and-known-effects).

DirectoryLink action records permit only the resource-specific fields required below; the operation's common graph facts remain subject to [Applied Lifecycle Topology and Operations](#applied-lifecycle-topology-and-operations).
Every record contains `kind`, `status`, and its listed resource-specific fields.
`expected_directory_link` predicates have exactly `kind`, `target_path`, and `link_target`; `missing` predicates have exactly `kind` and `target_path`.
Every effect object is the exact `directory_link` effect defined by the [Current State Schema](#current-state-schema).

| Action | Required fields and predicates | Forbidden fields | Verified Known update |
| --- | --- | --- | --- |
| `create_directory_link` | `resource_id`, `final_effect`, `precondition: missing(final target)`, `postcondition: expected_directory_link(final target)` | `old_effect`, `temporary`, `old_resource_id`, `new_resource_id`, except `superseded_resource_id` below | Record `final_effect` under `resource_id`. |
| Missing-target identity-handoff `create_directory_link` | The ordinary create fields plus `superseded_resource_id` naming the stale DirectoryLink identity at that same target | `old_effect`, `temporary`, `old_resource_id`, `new_resource_id` | Remove `superseded_resource_id` and record `final_effect` under `resource_id` atomically. Failed or uncertain create retains the stale Known record. |
| `remove_directory_link` | `resource_id`, `old_effect`, `precondition: expected_directory_link(old target)`, `postcondition: missing(old target)` | `final_effect`, `temporary`, `superseded_resource_id`, `old_resource_id`, `new_resource_id` | Remove `resource_id`. |
| `forget_missing` | `resource_id`, `old_effect`, `precondition: missing(old target)`, `postcondition: missing(old target)` | `final_effect`, `temporary`, `superseded_resource_id`, `old_resource_id`, `new_resource_id` | Remove `resource_id` without a target mutation. |
| `relocate_directory_link` | `resource_id`, `old_effect`, `final_effect`, `precondition: {"old": expected_directory_link, "new": missing}`, `postcondition: {"old": missing, "new": expected_directory_link}` | `temporary`, `superseded_resource_id`, `old_resource_id`, `new_resource_id` | Replace `resource_id`'s old effect with `final_effect`. |
| State-only `replace_ownership` | `resource_id` equal to `new_resource_id`, `old_resource_id`, `new_resource_id`, `old_effect`, `final_effect`, and matching `precondition`/`postcondition: expected_directory_link(shared target)` | `temporary`, `superseded_resource_id` | Remove `old_resource_id` and record `final_effect` under `new_resource_id` atomically. |
| Source-changing `replace_ownership` | `resource_id` equal to `new_resource_id`, `old_resource_id`, `new_resource_id`, `old_effect`, `final_effect`, old expected precondition, compound final postcondition, and `temporary` | `superseded_resource_id` | Remove `old_resource_id` and record `final_effect` under `new_resource_id` atomically. |
| `replace_directory_link` | `resource_id`, `old_effect`, `final_effect`, old expected precondition, compound final postcondition, and `temporary` | `superseded_resource_id`, `old_resource_id`, `new_resource_id` | Replace `resource_id`'s effect with `final_effect`. |

For every DirectoryLink action, `resource_id` is the affected final identity except that `forget_missing` and `remove_directory_link` use the stale identity.
`old_effect.target_path` and `final_effect.target_path` must agree with every corresponding predicate path.
`old_effect` and `final_effect` are forbidden where the table does not require them, including for an ordinary create.
The status is exactly one of the state-record statuses defined above.

The relocate predicates have this exact shape, with no additional keys:

```json
{
  "old": { "kind": "expected_directory_link", "target_path": "/home/example/.config/nvim", "link_target": "/home/example/dotfiles/nvim" },
  "new": { "kind": "missing", "target_path": "/home/example/.config/nvim-next" }
}
```

For a state-only handoff, `old_effect.target_path`, `final_effect.target_path`, and both predicate target paths are equal, and the exact link targets are equal.
For a source-changing handoff, the target paths are equal and the old/final link targets differ.

For `replace_directory_link`, the complete record contains the old and final `directory_link` effects, the old expected-directory-link precondition, the compound final postcondition, and this exact `temporary` shape:

```json
{
  "path": "/home/example/.config/.nvim.loadout-tmp-opaque",
  "predicate": {
    "kind": "expected_directory_link",
    "target_path": "/home/example/.config/.nvim.loadout-tmp-opaque",
    "link_target": "/home/example/dotfiles/nvim-next"
  }
}
```

`temporary.path` and `temporary.predicate.target_path` MUST be byte-for-byte equal.
For both `replace_directory_link` and source-changing `replace_ownership`, `temporary.predicate.link_target` MUST equal `final_effect.link_target` byte-for-byte in its exact normalized representation.
No other temporary fields are permitted.
Any record that violates either equality is invalid state and is rejected before recovery or target inspection.
The compound final postcondition has exactly this shape:

```json
{
  "target": {
    "kind": "expected_directory_link",
    "target_path": "/home/example/.config/nvim",
    "link_target": "/home/example/dotfiles/nvim-next"
  },
  "temporary": {
    "kind": "missing",
    "target_path": "/home/example/.config/.nvim.loadout-tmp-opaque"
  }
}
```

`postcondition.target.target_path` MUST equal `final_effect.target_path`, `postcondition.target.link_target` MUST equal `final_effect.link_target`, and `postcondition.temporary.target_path` MUST equal `temporary.path`.
The complete postcondition is true only when both nested predicates and containment, safe-parent, and declared-path association for both names are proved.
The temporary is an action-local sibling under the final target's safe parent, is not a declared resource target, and is the only temporary name the executor or recovery may address.
The operation record is durable before temporary creation; `running` is durable before that mutation.

A source-changing `replace_ownership` has the same predicates and temporary record, and additionally records `old_resource_id` and `new_resource_id`.
Its success update removes the old Known identity and records the final DirectoryLink under the new identity atomically with `succeeded`.
A `replace_directory_link` updates the existing resource identity to the final effect atomically with `succeeded`.

#### Directory-Link Replacement Rechecks

Immediately before temporary creation, the executor rechecks final-source real-directory safety, containment, the shared safe parent, target `expected_directory_link` for the recorded old link target, temporary `missing`, and declared-path association for target and temporary.
After creation, it verifies the temporary against its exact recorded expected-directory-link predicate and rechecks the required path association.

Immediately before the atomic name replacement, it repeats final-source safety, containment, shared-parent safety, target old-effect ownership, temporary expected-link ownership, and target/temporary declared-path association checks.
It MUST NOT replace after a failed recheck, delete the old target before replacement, or create an unrecorded temporary.
If the platform or filesystem cannot provide atomic same-filesystem name replacement that preserves the expected old link when that replacement fails, preflight rejects the action before a new operation record or target mutation.

After any replacement attempt, the executor observes the recorded final target and temporary and classifies from those recorded predicates, not the operating-system result alone.
The exact final postcondition authorizes the Known update and `succeeded`, including after an error return.
If the old target precondition still holds, failure is definite only after required temporary cleanup is proven complete.
Every different, unsafe, unavailable, or otherwise unprovable target/temporary observation is `uncertain` and retains the old Known effect and active-operation barrier.

#### Directory-Link Temporary Cleanup and Recovery

Cleanup may address only the exact recorded temporary sibling after a fresh no-follow recheck proves its expected directory-link predicate, safe parent, containment, and declared-path association.
It MUST NOT scan siblings, remove a different link or entry, follow the temporary link, remove its referent, or remove a parent.
If the temporary is already safely proved missing, cleanup is complete.
After an eligible removal attempt, cleanup is complete only when the exact recorded temporary is safely proved missing; an operating-system success or error return does not establish completion.

During execution and recovery, a complete final postcondition marks the action `succeeded` and performs the corresponding Known update.
When the old precondition holds, recovery or execution marks the action `failed` only after cleanup is complete and leaves the prior Known effect unchanged.
A different temporary, unsafe or unavailable observation, denied removal, a removal result whose missing aftermath cannot be proved, or any final observation that proves neither recorded predicate is `uncertain`.
It preserves the observed entry, retains the active operation, and does not retry, replan, restore, or otherwise mutate a target.
Recovery observes only recorded predicates and may perform only this exact temporary cleanup; it never resumes the old plan.

## Applied Lifecycle Topology and Operations

`applied_lifecycle_topology` is a sorted object with `current` and `historical` arrays. It retains only topology facts successfully committed with verified actions. A topology direct-edge fact is `{ "from": "<resource-id>", "to": "<resource-id>", "origin": "depends_on" }`; it sorts by `from`, `to`, and `origin`. A topology barrier fact is `{ "id": "stage/<stage-id>", "after": ["stage/<stage-id>"], "members": ["<resource-id>"], "origin": "stage" }`; its arrays are sorted and duplicate-free and barriers sort by `id`. A fact's source-array-qualified identity is unique: an identical direct edge or barrier ID MUST NOT appear in both arrays. Unknown fields, duplicates, noncanonical sorting, malformed references, or invalid cross-array identity are invalid state before recovery or target observation.

An active schema-5 operation contains exactly the graph and schedule selected before its first mutation: `actions`, `direct_edges`, `barriers`, and `schedule`, in addition to existing action preconditions and postconditions. Each action has a unique action ID, resource ID, typed action role, status, skip reason, topology authority, and planner-selected topology delta. `direct_edges` contains ordered action-ID pairs with `origin: "depends_on"`. Each barrier has stable ID `stage/<fully-qualified-stage-id>`, sorted `after` barrier IDs, sorted input and output action-ID sets, and provenance containing only canonical resolved IDs. `skip_reason` is null unless status is `skipped`; a skipped action is exactly `{ "kind": "dependency", "blocking_action_id": "<action-id>" }` or `{ "kind": "global_abort", "initiating_action_id": "<action-id>" }`. The state decoder rejects an unknown ID, missing action or barrier reference, duplicate ID or incidence, non-permutation schedule, invalid role/edge legality, status/reason mismatch, unsorted or duplicate collection, absent authority fact, or graph cycle before recovery or target observation.

Before a non-dry-run mutation, the repository durably records the complete graph, schedule, action pre/postconditions, statuses, skip facts, and planner-selected topology authority/delta. After the exact post-condition is verified, it atomically publishes only the action's Known update, `succeeded` status, and authorized topology delta. It cannot derive, add, merge, reorder, or transfer topology. Ordering-only changes have no action and therefore create no operation, topology update, or state rewrite.

A topology delta contains only sorted `add_current`, `remove_current`, `add_historical`, `remove_historical`, `replace_current`, and `replace_historical` arrays plus its authorizing action ID. Each replacement is `{ "before": <exact-barrier-fact>, "after": <exact-barrier-fact> }`: `before` must occur in the named source array, `after` must retain the same barrier ID with a strictly smaller sorted member array, and no delta may name one fact in more than one add, remove, or replacement position. Authority and every delta item contain exact facts; a removal may name only a fact present in its named source array.

A direct edge is published only by its successor action after the predecessor's required post-condition is represented by a prior atomic Known/action commit in this operation or pre-operation Known state. A barrier relation is published only by a successful output member after all its applicable input memberships are committed. Operation input and output action IDs become durable barrier resource memberships only after their associated actions succeed. Partial success thus retains only independently verified memberships and no unproven cross-resource relation. A stale operation stores an immutable sorted `stale_topology_snapshot`: all committed current and historical facts whose members or direct-edge endpoints are stale, together with the exact incident facts for each stale action. It reverses only selected all-stale relations from that snapshot and never derives stale ordering from changed declarations.

The planner assigns exactly one recorded removal owner for each snapshot fact, and recovery never transfers that authority. A direct edge is owned by its `to` endpoint's stale action; a multi-member barrier is owned successively by its member actions in reverse schedule order. A mixed fact incident to one stale action and one or more current resources has that stale action as sole owner and creates no stale scheduling edge. Each such barrier delta records the exact `before` and next `after` fact, replacing it while members remain and removing it only for the final member. A successful stale action atomically commits only its owned deletion or replacement, its verified Known update, and `succeeded`; failed or uncertain actions retain their topology facts and authority.

Recovery classifies an active operation from its recorded graph, predicates, authority, durable temporary/publication facts where applicable, and required current observations only. It does not read changed declarations, replan, or resume the old schedule. An unprovable outcome remains `uncertain` and blocks a fresh plan. A later apply recovers first and then creates a fresh plan.

## Locking

Every non-dry-run apply MUST acquire an exclusive operating-system lock associated with `state.lock` before it reads state for execution.
It holds the lock until it has closed or retained the active operation record after the apply attempt.

If the lock cannot be acquired, apply MUST fail without inspecting a managed target, recording progress, or mutating a target.
The lock implementation must not rely on a stale-file heuristic.
Process termination releases the operating-system lock according to platform semantics.

`validate`, `diff`, `plan`, and dry-run apply do not acquire this exclusive lock.
Read-only inspection commands also do not acquire it, create a lock file, recover an operation, or write state.

## Commit Protocol

Every state update, including an operation status transition, MUST be an atomic replacement of the complete state file:

1. Write the complete new state to a unique temporary file in the state directory.
2. Flush the temporary file.
3. Re-open, parse, and validate the written state.
4. Atomically replace `state.json` with the temporary file.
5. Flush the state directory when the platform supports it.

If the replacement fails, the previous `state.json` remains authoritative.
Loadout removes only its own temporary state file when cleanup is safe.
It MUST NOT remove target files, source files, profile files, or other user-visible artifacts as failure cleanup.

The state repository writes `running` before a resource mutation.
After a mutation, the executor verifies the resource post-condition; the repository atomically commits the Known-state update together with that action's `succeeded` status only after verification.
It does not update Known state before verification.

For a state-only `replace_ownership` action, the repository writes `running` before final no-follow verification and performs no target mutation.
When verification proves recorded old ownership and the shared expected link, copy, or DirectoryLink effect, it atomically replaces the old Known identity with the new one and marks the action `succeeded`.

## Mutation Result Classification

After an executor attempts a mutation, it MUST perform the action's no-follow post-mutation observation before deciding the operation result.
The observation, rather than the operating system call's success or error result alone, decides whether Known state may change.
For a create attempt that returns an error, the execution-time exception below takes precedence over the general post-condition row.

| Observation after an attempted mutation | Required result |
| --- | --- |
| The recorded post-condition holds exactly and the action's required success evidence is durable | Atomically commit the matching Known-state update and mark the action `succeeded`. |
| The recorded precondition still holds exactly | Mark the action `failed`; leave Known state unchanged. |
| Neither condition holds exactly, or observation is unsafe or unavailable | Mark the action `uncertain`; leave Known state unchanged. |

An action has no additional success evidence unless its action-specific contract defines it.
For sequential copy replacement and effect handoff, the required success evidence is `publication_attempted == true`.

During execution, a failed `create_link` or `create_copy` attempt followed by its expected final effect MUST remain `uncertain`, with Known unchanged. The observed effect can have been created externally after the final recheck, so its matching value does not establish successful creation by this attempt. This includes an already-existing-entry error and errors whose physical effects cannot be distinguished from external creation. A failed create whose recorded missing precondition still holds remains `failed`; unsafe or unavailable observations remain `uncertain`. This exception does not alter removal or replacement result classification.

The [external filesystem concurrency contract](file-link.md#external-filesystem-concurrency) applies to execution and recovery observations and cleanup. The state lock does not exclude unrelated filesystem mutation. After a removal race, `missing` at the recorded target can be indistinguishable from ordinary success: when every required recorded postcondition and path association holds, the result is `succeeded`, Known is removed, and apply can ultimately exit 0. This does not prove which entry was deleted and does not require a race diagnostic based on information unavailable to production observations.

Required observations include the recorded path association, not just the contents of a retained parent handle. They are not a globally atomic snapshot. If required facts cannot be established, retain uncertainty; postcondition verification does not restore atomic entry-identity or continuous-containment guarantees.

This rule covers permission, sharing, lock, read-only-filesystem, process-interruption, and other platform errors that occur after an action has been marked `running`.
It also applies if an operating system call reports an error but a later observation proves the recorded post-condition, subject to the failed-create execution exception above.
For a multi-step action such as `relocate_link`, the recorded precondition and post-condition include every required target observation; a partial relocation is therefore `uncertain`.

A `replace_link` action, and a `replace_ownership` action whose resolved link targets differ, also requires its recorded temporary sibling path to be `missing` as part of the complete post-condition.
When its old-target precondition still holds and the recorded temporary path is the exact temporary link, the executor or recovery may remove that temporary entry only after a fresh no-follow safety recheck.
It may mark the replacement `failed` only after that temporary path is `missing`.
An observed unexpected or unsafe temporary entry MUST NOT be removed. An unremovable temporary or unprovable cleanup aftermath makes the replacement `uncertain`. Cleanup is limited to the exact recorded path, with no sibling scanning, and shares the recheck-to-syscall limitation. A failed recheck after temporary creation does not imply that no mutation occurred; existing effects are classified using the recorded predicates.

When an action-specific copy classification requires eligible staged-temporary cleanup before it can become `failed` or `failed` with a conflict, cleanup is complete only when the recorded temporary pathname is freshly proven `missing`.
The temporary may already be missing, or it may become missing after a permitted removal attempt; an operating-system success or error result alone does not decide completion.

| Required staged-temporary cleanup observation | Required result |
| --- | --- |
| Recorded temporary is safely observed missing before cleanup, or is safely observed missing after an eligible removal attempt | Cleanup is complete; apply the recorded final-target classification. |
| Eligible removal is denied or reports an error, and missing cannot be proven afterward | `uncertain`; retain the action and Known state. |
| An eligible removal attempt reports success but the recorded temporary is not proven missing afterward | `uncertain`; retain the action and Known state. |
| The recorded temporary is present but different, ineligible, unsafe, or unavailable | Do not remove it; mark `uncertain` and retain the action and Known state. |

An `uncertain` cleanup aftermath retains the active operation as a global barrier.
An operator may correct the exact recorded temporary artifact manually, after which recovery re-observes the recorded temporary and final-target predicates; it does not infer cleanup from a prior removal attempt.

For sequential copy replacement and effect handoff, the preceding general table is refined as follows during both execution and recovery. A `replace_copy`, copy `replace_ownership` with changed bytes, or `link -> copy` action whose final copy postcondition holds and whose recorded `publication_attempted` fact is true is `succeeded`. If its old-effect precondition still holds, it is `failed` after eligible-temporary cleanup. If its final target is safely observed as missing, it is `failed` after eligible-temporary cleanup; Known remains the old effect and a fresh plan may create the desired copy at the missing target. If the final target is a safely observed entry matching neither predicate, it is `failed` with a conflict and does not mutate that entry. Unsafe or unavailable observation remains `uncertain`.

The same final-observation classification applies to `copy -> link`, except that it has no staged copy temporary. Its final-link postcondition with `publication_attempted` is `succeeded`; its old-copy precondition or a safely observed missing final target is `failed`; a safely observed different entry is `failed` with a conflict; and unsafe or unavailable observation is `uncertain`.
A safely observed different-final conflict does not itself require staged-temporary cleanup; an unexpected or ineligible temporary is preserved and makes the action `uncertain` whenever cleanup is required.
These sequential actions do not restore an old effect, back it up, or roll it back. This exception does not change `replace_link` or source-changing `replace_ownership`, whose temporary cleanup and old-effect-preservation rules remain in force.

## Failure and Recovery

Before a non-dry-run apply creates its fresh plan, it reconciles any `active_operation` while holding the exclusive lock.
Recovery observes only the paths recorded in that operation record; it does not execute the old plan or mutate a declared resource target.
It may perform only the replacement-temporary cleanup explicitly authorized by Mutation Result Classification.

For each unfinished action, recovery applies the same evidence rules:

| Action status and observation | Recovery result |
| --- | --- |
| `pending` | Mark `skipped`; leave Known state unchanged. |
| `running`; recorded post-condition holds exactly and the action's required success evidence is durable | Commit the corresponding Known-state update and mark `succeeded`. |
| `running`; recorded precondition still holds exactly | Mark `failed`; leave the prior Known state unchanged. |
| `running`; neither condition holds exactly, or inspection is unsafe | Mark `uncertain`; retain Known state unchanged. |

An unfinished `create_link` or `create_copy` is an exception to the recorded-postcondition row. Whether its status is `running` or `uncertain`, an observed expected final effect remains `uncertain` and Known remains unchanged. That observation cannot prove that Loadout created the effect: it is indistinguishable from external creation after the final recheck, including when the create syscall returned success before the process stopped. If the recorded create target is `missing`, recovery marks the action `failed`; every other observation remains `uncertain`. This deliberately sacrifices automatic recovery of a successful create whose Known commit was interrupted in order to preserve the rule that Loadout does not adopt an unmanaged matching effect.

For `relocate_link`, both required final observations must hold to prove success.
A partial relocation, such as both old and new links existing, is `uncertain`.

`relocate_copy` is also a two-target action. It succeeds only when its complete recorded new-copy and old-target-missing postcondition holds, and fails only when its complete recorded precondition still holds. Any partial relocation remains `uncertain`; the single-target missing-final exception for sequential copy replacement does not apply.

For a running state-only `replace_ownership` action, recovery commits the recorded identity handoff only when the old Known resource identity remains present and Actual observation proves the recorded shared expected effect.
That recovery performs no target mutation.
This covers equal resolved file-link or DirectoryLink targets and copy effects with the same target and applied fingerprint.
For a copy `replace_ownership` action with changed bytes, recovery instead uses the sequential copy-replacement classification, including `temporary_staged`, `publication_attempted`, and the old-to-new Known identity update on success.
Any other observation follows the normal recorded-precondition and post-condition classification.

After every action has a final status of `succeeded`, `failed`, or `skipped`, the repository removes `active_operation` in a final atomic commit.
If any action is `uncertain`, the repository retains the operation record and apply returns a blocking diagnostic without creating a new plan.
An active operation containing `uncertain` is a global barrier: it prevents every fresh plan and every new planned target mutation, including work for resources not named by the uncertain action.
Read-only inspection may report the active operation but MUST NOT reconcile it or use it to permit a partial apply.

An operator may correct the filesystem manually.
A later apply re-runs recovery and proceeds only if every formerly uncertain action can then be proven successful or failed by its recorded conditions. For `create_link` and `create_copy`, only the recorded missing precondition can close an uncertain action; a matching final effect remains open.

Verified actions from before a failure remain in Known state; recovery does not roll them back.
