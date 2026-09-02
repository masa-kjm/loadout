# Lifecycle Specification

## Scope

This specification defines the v0.2.0 lifecycle for validating declarations, inspecting drift, producing a plan, and applying a plan.
It owns planning decisions and the Desired/Known/Actual transition table.
The File Links and State and Recovery specifications define the filesystem and durable-state mechanics used by those decisions.

## Inputs

The planner is a pure function:

```text
Resolved Desired + Known + Actual -> Plan
```

Its inputs have distinct meanings:

- **Resolved Desired** is the canonical resource set for the selected root profile.
- **Known** is the durable record of verified effects.
- **Actual** is a no-follow observation of the current target and relevant parent paths.

The planner MUST NOT access the filesystem, read a store, write state, acquire a lock, execute a process, or format terminal output.

## Validate

Profile validation parses the environment configuration and discovered profiles, resolves a selected root profile when one is supplied, and performs the static checks defined by Configuration and Profiles.
It does not inspect resource targets and makes no mutation.

## Diff

Diff compares Known state with Actual state for every resource recorded in Known state.
It uses the same no-follow Actual-state observation contract as planning, but it does not resolve Desired state, invoke the planner, reconcile an active operation, or mutate state or filesystem entries.

Diff reports observation results and unfinished operation status only.
It does not classify drift into repair actions; plan is the command that derives actions from Resolved Desired, Known, and Actual state.
The CLI specification owns the `diff` command syntax, output, and exit-status behavior.

## Plan

A Plan contains a deterministic sequence of planned actions, action reasons, and diagnostics.
It is either:

- **executable**, when it has no blocking diagnostics; or
- **blocked**, when any conflict, invalid precondition, or unsupported platform condition prevents safe execution.

A blocked Plan is a successful planning result, but apply MUST NOT mutate from it.
Conflicts are not actions.

### Planned Actions

The v0.2.0 planner may produce these actions:

| Action | Meaning |
| --- | --- |
| `create_link` | Create the desired link at a missing target. |
| `replace_link` | Replace an expected managed link with a link to a new source at the same target. |
| `relocate_link` | Create a new link at a new target, then remove the old expected link. |
| `replace_ownership` | Replace an expected stale managed link at a target with a new resource identity. |
| `remove_link` | Remove an expected stale managed link. |
| `forget_missing` | Remove a stale Known-state record whose target is already missing. |
| `noop` | Record that the desired resource is already satisfied. |

Each action identifies its fully qualified resource ID, resolved target or targets, expected precondition, required post-condition, and reason.
The executor completes every step required by one action's post-condition before it begins another action.
An action is never divided across execution phases.

### Transition Table

The following table fixes the required planning decisions for a file-link resource.
`expected` means an `expected_link` matching the Known-state record.
`unexpected` means `matching_unmanaged_link`, `other_link`, `other_entry`, or `unsafe_path`.

| Desired | Known | Actual | Required result |
| --- | --- | --- | --- |
| Present; no previous identity | Absent | Target missing | `create_link` |
| Present; no previous identity | Absent | Any target entry | Blocked conflict; never adopt it |
| Present; unchanged definition | Present | Expected | `noop` |
| Present; unchanged definition | Present | Target missing | `create_link` |
| Present; unchanged definition | Present | Unexpected | Blocked conflict |
| Present; source changed, same target | Present | Expected | `replace_link` |
| Present; source changed, same target | Present | Target missing | `create_link` |
| Present; source changed, same target | Present | Unexpected | Blocked conflict |
| Present; target changed | Present | Old target expected and new target missing | `relocate_link` |
| Present; target changed | Present | Any other old or new target observation | Blocked conflict |
| Absent | Present | Expected | `remove_link` |
| Absent | Present | Target missing | `forget_missing` |
| Absent | Present | Unexpected | Blocked conflict |
| New identity claims a target held by a stale identity | Old identity present | Old target expected | `replace_ownership` |
| Multiple desired identities claim one normalized target | Any | Any | Blocked conflict |

For `replace_ownership`, the old resource must be stale, the new resource source must validate, and the target must be the old resource's expected link.
The action records the old and new fully qualified resource IDs and their resolved link targets.
When those link targets are equal, it is a state-only managed identity handoff; when they differ, it replaces the old managed link with the new expected link.
In either case, it updates Known state from the old resource ID to the new one only after the required link post-condition is verified.
The File Links and State and Recovery specifications define the corresponding mutation, no-mutation, and recovery-record requirements.

A source file content change at the same resolved source path does not create an action.
A file link always exposes the current source content; v0.2.0 does not fingerprint source content for link resources.

## Preflight

Apply performs preflight after creating a fresh plan and before recording an action as running.
Preflight MUST verify all planned actions without mutation:

- the plan is executable;
- every current source still exists and satisfies the File Links source contract;
- every target and target parent satisfies the planned precondition;
- no target collision exists;
- the platform supports each planned mutation;
- the state repository is writable; and
- the exclusive state lock is held.

If preflight fails, apply reports blocking diagnostics and does not create an operation record or mutate a target.

## Apply

A non-dry-run apply MUST perform this sequence:

1. Acquire the exclusive state lock.
2. Load state and reconcile an incomplete prior operation as defined by State and Recovery.
3. Resolve and validate the current selected root profile.
4. Load Known state and inspect relevant Actual state.
5. Generate a fresh Plan.
6. Run preflight.
7. Present the executable Plan and obtain confirmation as defined by the CLI specification.
8. Persist the new operation record.
9. Execute actions sequentially.
10. Verify the post-condition of each completed action.
11. Atomically commit the verified Known-state update and resource progress.
12. Close the operation record when every action has a final status.

Apply MUST NOT resume a prior plan by its stored action sequence.
After recovery, it always plans from current Resolved Desired, Known, and Actual state.

Confirmation is requested only after successful preflight.
If confirmation is declined or unavailable, apply creates no operation record and performs no target mutation.

The executor must recheck filesystem safety immediately before each mutation.
A failed recheck aborts the action and records failure or uncertainty; it never causes the executor to choose a different action.

## Execution Order

Apply is sequential in v0.2.0.
Within every phase, actions are ordered lexicographically by their fully qualified resource ID.
For `replace_ownership`, the sort key is `<old-resource-id>\u0000<new-resource-id>`.

v0.2.0 has no user-configurable per-resource execution order.
Fully qualified resource ID ordering is a deterministic tie-breaker, not an ordering interface; resource IDs express identity and users MUST NOT choose or rename them to control execution order.
Future ordering constraints require a separate dependency contract and must not change the meaning of the v0.2.0 profile declaration order.

The phases are:

1. `create_link`;
2. `replace_link`, `replace_ownership`, and `relocate_link`;
3. `remove_link` and `forget_missing`.

A `relocate_link` action creates and verifies the new target, then removes and verifies the old target, as one contiguous phase-2 action.
No other action begins between those steps.

An action in a later phase is not attempted after an earlier action fails.
Verified actions from earlier phases remain committed; v0.2.0 does not roll them back.

## Dry Run

`apply --dry-run` performs resolution, validation, observation, and planning only.
It reports the same Planned Actions and blocking diagnostics as a non-dry-run apply at the point of observation.
It MUST NOT acquire an exclusive state lock, create an operation record, write state, create directories, create temporary files, or mutate a target.
