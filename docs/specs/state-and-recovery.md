# State and Recovery Specification

## Scope

This specification defines v0.2.0 durable state, operation records, exclusive locking, atomic commits, and recovery after interruption.
It applies only to the v0.2.0 file-link lifecycle.

## State Files

The state repository owns these files in the platform state directory defined by [Configuration](configuration.md):

```text
state.json
state.lock
```

`state.json` is UTF-8 JSON.
If it does not exist, Loadout starts with an empty v0.2.0 state.
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
| `definition_hash` | SHA-256 hash of the canonical resolved declaration, excluding source file content. |
| `source_path` | Verified resolved source path at the successful application. |
| `target_path` | Resolved target path at the successful application. |
| `link_target` | Exact normalized absolute symbolic-link target created by Loadout. |

The key and `target_path` are unique across `resources`.
All stored paths are absolute, normalized for the current platform, and contain no home shorthand or relative components.

Known state records only post-conditions that were verified after an operation.
It is not a substitute for actual filesystem inspection before a later destructive action.

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
`desired_hash` identifies the Resolved Desired set used to produce the original plan.
`actions` is an object keyed by an opaque action ID; each action records enough resolved preconditions and post-conditions for recovery without rereading the old profile.

For a replacement or removal action, the precondition records the old expected link target.
For a replacement action, the record also contains its unique action-local temporary sibling path and expected temporary link target.
This path is not a declared resource target and is used only to complete or safely clean up that replacement.
For a relocate action, the record includes both old and new targets and their required final observations.
For ownership replacement, the record includes the old and new fully qualified resource IDs.

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

## Mutation Result Classification

After an executor attempts a mutation, it MUST perform the action's no-follow post-mutation observation before deciding the operation result.
The observation, rather than the operating system call's success or error result alone, decides whether Known state may change.

| Observation after an attempted mutation | Required result |
| --- | --- |
| The recorded post-condition holds exactly | Atomically commit the matching Known-state update and mark the action `succeeded`. |
| The recorded precondition still holds exactly | Mark the action `failed`; leave Known state unchanged. |
| Neither condition holds exactly, or observation is unsafe or unavailable | Mark the action `uncertain`; leave Known state unchanged. |

This rule covers permission, sharing, lock, read-only-filesystem, process-interruption, and other platform errors that occur after an action has been marked `running`.
It also applies if an operating system call reports an error but a later observation proves the recorded post-condition.
For a multi-step action such as `relocate_link`, the recorded precondition and post-condition include every required target observation; a partial relocation is therefore `uncertain`.

A replacement action's complete post-condition also requires its recorded temporary sibling path to be `missing`.
When its old-target precondition still holds and the recorded temporary path is the exact temporary link, the executor or recovery may remove that temporary entry only after a fresh no-follow safety recheck.
It may mark the replacement `failed` only after that temporary path is `missing`.
An unexpected, unsafe, or unremovable temporary entry makes the replacement `uncertain`; Loadout leaves that entry untouched.

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

For `relocate_link`, both required final observations must hold to prove success.
A partial relocation, such as both old and new links existing, is `uncertain`.

After every action has a final status of `succeeded`, `failed`, or `skipped`, the repository removes `active_operation` in a final atomic commit.
If any action is `uncertain`, the repository retains the operation record and apply returns a blocking diagnostic without creating a new plan.

An operator may correct the filesystem manually.
A later apply re-runs recovery and proceeds only if every formerly uncertain action can then be proven successful or failed by its recorded conditions.

Verified actions from before a failure remain in Known state.
v0.2.0 does not roll them back.
