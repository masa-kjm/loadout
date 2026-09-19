# State and Recovery Specification

## Scope

This specification defines v0.3.0 durable state, operation records, exclusive locking, atomic commits, and recovery after interruption retained by v0.4.0.
It applies only to the v0.3.0 file-link lifecycle retained by v0.4.0.

Every normative v0.3.0 requirement in this document remains a v0.4.0 requirement under the [retained baseline](README.md#retained-v030-baseline), including state validation before v0.4.0 inspection relies on state facts.

## State Files

The state repository owns these files in the platform state directory defined by [Configuration](configuration.md):

```text
state.json
state.lock
```

`state.json` is UTF-8 JSON.
If it does not exist, Loadout starts with an empty v0.3.0 state.
The state directory and lock file may be created by a non-dry-run apply.

If `state.json` is unreadable, invalid JSON, has an unsupported schema version, or violates an invariant, Loadout MUST abort before it inspects a managed target or creates a mutation.
It MUST NOT repair, migrate, or overwrite the state automatically.

## Known State Schema

The following is the logical state schema.
The order of object members has no meaning.

```json
{
  "schema_version": 1,
  "resources": {
    "base/git-config": {
      "definition_hash": "sha256:...",
      "file_link": {
        "source_path": "/home/example/dotfiles/git/config",
        "target_path": "/home/example/.gitconfig",
        "link_target": "/home/example/dotfiles/git/config"
      }
    }
  },
  "active_operation": null
}
```

`schema_version` is REQUIRED and MUST be `1`.
`resources` is REQUIRED and is an object keyed by fully qualified resource ID.
`active_operation` is REQUIRED and is either `null` or an operation record.
Unknown fields are errors at every object level.

Each Known file-link resource contains:

| Field | Meaning |
| --- | --- |
| `definition_hash` | `sha256:<lowercase-hex>` hash of the canonical resolved declaration defined in [Canonical Hashes](#canonical-hashes), excluding source file content. |
| `source_path` | Verified resolved source path at the successful application. |
| `target_path` | Resolved target path at the successful application. |
| `link_target` | Exact normalized absolute symbolic-link target created by Loadout. |

The key and `target_path` are unique across `resources`.
All stored paths are absolute, normalized for the current platform, and contain no home shorthand or relative components.
On Windows, every stored path and every path in an active operation must satisfy the normal DOS or ordinary UNC representation in [Windows Path Representation](file-link.md#windows-path-representation).
Schema version 1 has no implicit path migration: a document whose Windows paths are not in that representation, or whose recorded hash no longer matches its exact normalized path values, is rejected before target inspection, planning, or state mutation.
Loadout does not rewrite a state document, recalculate a hash, or reinterpret an active operation to accept an alias.

Known state records only post-conditions that were verified after an operation.
It is not a substitute for actual filesystem inspection before a later destructive action.

## Canonical Hashes

`definition_hash` and `desired_hash` use the string form `sha256:<lowercase-hex>`.
Their hash input is the UTF-8 encoding of a JSON Canonicalization Scheme (JCS; RFC 8785) value.
They MUST NOT be derived from YAML, JSON, or other source-document serialization.

For a v0.3.0 file-link resource, the canonical value for `definition_hash` is exactly this object, using the resource's resolved values:

```json
{
  "format": "loadout.resolved-file-link.v1",
  "kind": "file",
  "operation": "link",
  "source_path": "/home/example/dotfiles/git/config",
  "target_path": "/home/example/.gitconfig",
  "type": "file"
}
```

`source_path` and `target_path` are the absolute, platform-normalized paths in Resolved Desired.
No additional path, case, separator, or Unicode normalization is applied while hashing.
The definition representation does not contain a resource ID; fully qualified resource ID is resource identity rather than definition content.

The canonical value for `desired_hash` is exactly this object.
Its `resources` array contains resource objects sorted lexicographically by `resource_id`, and each `definition` value is the preceding definition object:

```json
{
  "format": "loadout.resolved-desired.v1",
  "resources": [
    {
      "definition": {
        "format": "loadout.resolved-file-link.v1",
        "kind": "file",
        "operation": "link",
        "source_path": "/home/example/dotfiles/git/config",
        "target_path": "/home/example/.gitconfig",
        "type": "file"
      },
      "resource_id": "base/git-config"
    }
  ]
}
```

`resource_id` is the fully qualified resource ID and is a `desired_hash` input only.
Document schema versions, store IDs, profile-discovery order, raw declaration syntax, and source file content are not hash inputs.
The v0.3.0 state schema fixes these canonical representations.
Changing either representation requires a new state schema and explicit migration; Loadout MUST NOT silently recalculate an existing hash with a different representation.

## Operation Record

An operation record describes work that has begun but may not be complete.

```json
{
  "id": "01JEXAMPLE...",
  "desired_hash": "sha256:...",
  "actions": {
    "a1": {
      "kind": "create_link",
      "resource_id": "base/git-config",
      "precondition": { "target": "missing" },
      "postcondition": {
        "target": "expected_link",
        "link_target": "/home/example/dotfiles/git/config"
      },
      "status": "pending"
    }
  }
}
```

`id` is a newly generated opaque operation ID.
`desired_hash` is the [Canonical Hashes](#canonical-hashes) value that identifies the Resolved Desired set used to produce the original plan.
`actions` is an object keyed by an opaque action ID; each action records enough resolved preconditions and post-conditions for recovery without rereading the old profile.

For `replace_link`, `remove_link`, and `replace_ownership`, the precondition records the old expected link target.
For `replace_link`, and for `replace_ownership` whose resolved link targets differ, the record also contains its unique action-local temporary sibling path and expected temporary link target.
This path is not a declared resource target and is used only to complete or safely clean up that replacement.
For a relocate action, the record includes both old and new targets and their required final observations.
For `replace_ownership`, the record includes `old_resource_id`, `new_resource_id`, the resolved target path, and both the old and new expected link targets.
The temporary sibling fields are absent when the old and new resolved link targets are equal.

Each action status is one of:

| Status | Meaning |
| --- | --- |
| `pending` | The action has not started. |
| `running` | The start was committed, but completion is not known. |
| `succeeded` | The required post-condition was verified and the corresponding Known state update was committed. |
| `failed` | The action returned a definite failure while its recorded precondition or Known state remained valid. |
| `skipped` | The action was not attempted because an earlier action ended the operation. It has no effect. |
| `uncertain` | Actual observation cannot prove either the recorded precondition or post-condition. |

`pending`, `running`, and `uncertain` are not successful states.

## Locking

Every non-dry-run apply MUST acquire an exclusive operating-system lock associated with `state.lock` before it reads state for execution.
It holds the lock until it has closed or retained the active operation record after the apply attempt.

If the lock cannot be acquired, apply MUST fail without inspecting a managed target, recording progress, or mutating a target.
The lock implementation must not rely on a stale-file heuristic.
Process termination releases the operating-system lock according to platform semantics.

`validate`, `diff`, `plan`, and dry-run apply do not acquire this exclusive lock.
The v0.4.0 inspection commands also do not acquire it, create a lock file, recover an operation, or write state.

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
After a successful mutation, it verifies the resource post-condition and atomically commits the Known-state update together with that action's `succeeded` status.
It does not update Known state before verification.

For a `replace_ownership` action whose resolved link targets are equal, the state repository writes `running` before the final no-follow verification.
It performs no filesystem mutation.
When that verification proves the recorded old ownership and shared expected link, it atomically replaces the old Known resource identity with the new one together with the action's `succeeded` status.

## Mutation Result Classification

After an executor attempts a mutation, it MUST perform the action's no-follow post-mutation observation before deciding the operation result.
The observation, rather than the operating system call's success or error result alone, decides whether Known state may change.
For a create attempt that returns an error, the execution-time exception below takes precedence over the general post-condition row.

| Observation after an attempted mutation | Required result |
| --- | --- |
| The recorded post-condition holds exactly | Atomically commit the matching Known-state update and mark the action `succeeded`. |
| The recorded precondition still holds exactly | Mark the action `failed`; leave Known state unchanged. |
| Neither condition holds exactly, or observation is unsafe or unavailable | Mark the action `uncertain`; leave Known state unchanged. |

During execution, a failed `create_link` attempt followed by `expected_link` MUST remain `uncertain`, with Known unchanged. The observed link can have been created externally after the final recheck, so its matching value does not establish successful creation by this attempt. This includes an already-existing-entry error and errors whose physical effects cannot be distinguished from external creation. A failed create whose recorded missing precondition still holds remains `failed`; unsafe or unavailable observations remain `uncertain`. This exception does not alter removal or replacement result classification.

The [external filesystem concurrency contract](file-link.md#external-filesystem-concurrency) applies to execution and recovery observations and cleanup. The state lock does not exclude unrelated filesystem mutation. After a removal race, `missing` at the recorded target can be indistinguishable from ordinary success: when every required recorded postcondition and path association holds, the result is `succeeded`, Known is removed, and apply can ultimately exit 0. This does not prove which entry was deleted and does not require a race diagnostic based on information unavailable to production observations.

Required observations include the recorded path association, not just the contents of a retained parent handle. They are not a globally atomic snapshot. If required facts cannot be established, retain uncertainty; postcondition verification does not restore atomic entry-identity or continuous-containment guarantees.

This rule covers permission, sharing, lock, read-only-filesystem, process-interruption, and other platform errors that occur after an action has been marked `running`.
It also applies if an operating system call reports an error but a later observation proves the recorded post-condition, subject to the failed-create execution exception above.
For a multi-step action such as `relocate_link`, the recorded precondition and post-condition include every required target observation; a partial relocation is therefore `uncertain`.

A `replace_link` action, and a `replace_ownership` action whose resolved link targets differ, also requires its recorded temporary sibling path to be `missing` as part of the complete post-condition.
When its old-target precondition still holds and the recorded temporary path is the exact temporary link, the executor or recovery may remove that temporary entry only after a fresh no-follow safety recheck.
It may mark the replacement `failed` only after that temporary path is `missing`.
An observed unexpected or unsafe temporary entry MUST NOT be removed. An unremovable temporary or unprovable cleanup aftermath makes the replacement `uncertain`. Cleanup is limited to the exact recorded path, with no sibling scanning, and shares the recheck-to-syscall limitation. A failed recheck after temporary creation does not imply that no mutation occurred; existing effects are classified using the recorded predicates.

## Failure and Recovery

Before a non-dry-run apply creates its fresh plan, it reconciles any `active_operation` while holding the exclusive lock.
Recovery observes only the paths recorded in that operation record; it does not execute the old plan or mutate a declared resource target.
It may perform only the replacement-temporary cleanup explicitly authorized by Mutation Result Classification.

For each unfinished action, recovery applies the same evidence rules:

| Action status and observation | Recovery result |
| --- | --- |
| `pending` | Mark `skipped`; leave Known state unchanged. |
| `running`; recorded post-condition holds exactly | Commit the corresponding Known-state update and mark `succeeded`. |
| `running`; recorded precondition still holds exactly | Mark `failed`; leave the prior Known state unchanged. |
| `running`; neither condition holds exactly, or inspection is unsafe | Mark `uncertain`; retain Known state unchanged. |

An unfinished `create_link` is an exception to the recorded-postcondition row. Whether its status is `running` or `uncertain`, an observed `expected_link` remains `uncertain` and Known remains unchanged. That observation cannot prove that Loadout created the link: it is indistinguishable from external creation after the final recheck, including when the create syscall returned success before the process stopped. If the recorded create target is `missing`, recovery marks the action `failed`; every other observation remains `uncertain`. This deliberately sacrifices automatic recovery of a successful create whose Known commit was interrupted in order to preserve the rule that Loadout does not adopt an unmanaged matching link.

For `relocate_link`, both required final observations must hold to prove success.
A partial relocation, such as both old and new links existing, is `uncertain`.

For a running `replace_ownership` action whose resolved link targets are equal, recovery commits the recorded identity handoff only when the old Known resource identity remains present and Actual observation proves the recorded shared expected link.
That recovery performs no target mutation.
Any other observation follows the normal recorded-precondition and post-condition classification.

After every action has a final status of `succeeded`, `failed`, or `skipped`, the repository removes `active_operation` in a final atomic commit.
If any action is `uncertain`, the repository retains the operation record and apply returns a blocking diagnostic without creating a new plan.

An operator may correct the filesystem manually.
A later apply re-runs recovery and proceeds only if every formerly uncertain action can then be proven successful or failed by its recorded conditions. For `create_link`, only the recorded missing precondition can close an uncertain action; a matching link remains open.

Verified actions from before a failure remain in Known state.
v0.3.0 does not roll them back.
