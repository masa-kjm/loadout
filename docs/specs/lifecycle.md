# Lifecycle Specification

## Scope

This specification defines the current lifecycle for validating declarations, inspecting drift, producing a plan, and applying a plan.
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

The transition tables in this document, [File Copies](file-copy.md#planning), and [Directory Links](directory-link.md#planning-transitions) define the selectable actions for each effect. Cross-effect file handoffs use `replace_effect` as specified in [File-Copy and Cross-Effect Planning](#file-copy-and-cross-effect-planning).

Each action identifies its fully qualified resource ID, resolved target or targets, expected precondition, required post-condition, and reason.
The executor completes every step required by one action's post-condition before it begins another action.
An action is indivisible for scheduling and execution.

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

A source file content change at the same resolved source path does not create a file-link action; [File Links](file-link.md) do not fingerprint source content.

## Preflight

Apply performs preflight after creating a fresh plan and before recording an action as running.
Preflight MUST verify all planned actions without mutation:

- the plan is executable;
- every current source still satisfies its effect-specific source contract and, for a copy, still has the planned content fingerprint;
- every target and target parent satisfies the planned precondition;
- no target collision exists;
- the platform supports each planned mutation;
- the state repository is writable; and
- the exclusive state lock is held.

If preflight fails, apply reports blocking diagnostics and does not create a new operation record or perform new target mutation. Recovery of a prior operation occurs before this preflight and may already have performed its explicitly permitted cleanup or state commits.

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
If recovery retains any `uncertain` action, it MUST stop before resolution, observation for a fresh plan, preflight, confirmation, a new operation record, or any new planned target mutation.
This is a global barrier, not resource-scoped continuation or partial apply.

Confirmation is requested only after successful preflight.
If confirmation is declined or unavailable, apply creates no new operation record and performs no new target mutation; this does not undo permitted earlier recovery effects.

The executor must recheck filesystem safety immediately before each mutation.
A failed recheck prevents the next mutation step and records failure or uncertainty from the recorded predicates; it never causes the executor to choose a different action. Earlier verified actions remain committed, and earlier steps such as temporary creation or relocation creation may have effects requiring classification. The [external filesystem concurrency contract](file-link.md#external-filesystem-concurrency) applies to all checks and attempts; rejection of observable changes does not exclude a race after the last recheck.

## Execution Order

Apply is sequential in v0.8.0. The planner first classifies normal lifecycle actions, then constructs and validates one combined DAG. The relation contains only typed legal lifecycle/replacement relations, canonical direct `depends_on` prerequisites, compact stage barriers, and permissible historical stale-removal topology. The planner rejects every combined cycle before operation creation or target mutation. An unsupported action-role pair is a blocking diagnostic with no fallback.

The only action roles are `current_create`, `current_change`, `current_handoff`, `stale_remove`, and `forget_missing`. Current-to-current direct and forward-stage relations are legal. `stale_remove` and `forget_missing` are stale actions and may relate only by reversing a selected applied-topology edge whose endpoints are stale. Current-to-stale, stale-to-current, and every unspecified pair are illegal. Resource-specific replacement or handoff predicates remain additional required safety predicates and never override this closed legality table.

The Plan stores unique actions, sorted direct action edges with `origin: "depends_on"`, compact barrier units, and a schedule that is a permutation of the action IDs. A barrier has stable ID `stage/<fully-qualified-stage-id>`, sorted `after` barrier IDs, sorted input/output action-ID sets, and provenance containing only canonical resolved IDs. Barrier stages MUST NOT be serialized as an n-by-m cross-product of action edges. Direct edges, barrier incidences, action IDs, and schedule members are unique and canonical. A graph with an unknown reference, duplicate or noncanonical collection, invalid status/reason relation, invalid role relation, missing scheduled action, or cycle is invalid before recovery or target observation.

The schedule is a deterministic sequential topological order. Independent ready actions use fully qualified resource ID only as a tie-breaker. YAML mapping order, include order, declaration order, collection order, source order, and lexical renaming are not ordering APIs. An ordering-only declaration change creates no resource action, active operation, applied-topology update, or state rewrite. An unstaged resource creates no barrier membership. A satisfied prerequisite creates no synthetic action.

A failed or uncertain action stops every later planned mutation. Every unstarted action reachable from an unsatisfied or non-success terminal predecessor records a dependency skip naming that blocking action. Every other unstarted action records a global-abort skip naming the first failed or uncertain action that stopped execution. Verified earlier effects are not rolled back.

## Dry Run

`apply --dry-run` performs resolution, validation, observation, and planning only.
It reports the same Planned Actions and blocking diagnostics as a non-dry-run apply at the point of observation.
It MUST NOT acquire an exclusive state lock, create an operation record, write state, create directories, create temporary files, or mutate a target.

## File-Copy and Cross-Effect Planning

The planner receives source fingerprints as part of Resolved Desired copy values.

The actions are `create_link`, `replace_link`, `relocate_link`, `remove_link`, `create_copy`, `replace_copy`, `relocate_copy`, `remove_copy`, `replace_ownership`, `replace_effect`, `forget_missing`, and `noop`.
`replace_effect` is the managed same-target handoff between `link` and `copy`.
It requires current Actual proof of the old Known effect and uses the final effect's publication contract; it never adopts an unmanaged entry.

| Desired effect | Known effect | Actual | Required result |
| --- | --- | --- | --- |
| Copy at unchanged target | Link | Expected old link | `replace_effect` with `old_effect: file_link`, `final_effect: file_copy` |
| Link at unchanged target | Copy | Expected old copy | `replace_effect` with `old_effect: file_copy`, `final_effect: file_link` |
| Copy at unchanged target under a new resource identity | Copy under a stale resource identity | Expected old copy | `replace_ownership` with `old_effect: file_copy`, `final_effect: file_copy` |
| Copy at unchanged target under a new resource identity | Copy under a stale resource identity | Missing | `create_copy` that supersedes the stale identity on success |
| Either final effect | Either old effect | Missing | Corresponding final-effect create; old Known remains until that action's normal state update rules apply |
| Either final effect | Either old effect | Any unexpected or unsafe observation | Blocked conflict |

For a handoff, the typed precondition is the complete old Known effect and the typed postcondition is the complete final effect. Neither handoff is state-only or adopts an unmanaged entry. [Link-to-Copy Effect Handoff](file-copy.md#link-to-copy-effect-handoff) and [Copy-to-Link Effect Handoff](file-link.md#copy-to-link-effect-handoff) own their respective mutation and failure aftermath; [State and Recovery](state-and-recovery.md#file-effect-action-records) owns the action record and atomic Known update.

A managed identity handoff requires a stale old resource identity, exactly one new Desired identity at the same normalized target, and current Actual proof of the old Known effect.
For `file_copy` effects, the new Desired source is resolved and fingerprinted before planning.
When the final copy fingerprint equals the old applied fingerprint, `replace_ownership` is state-only: after a final expected-copy observation, it replaces the old Known identity with the new one without allocating a temporary or mutating the target.
When the fingerprints differ, `replace_ownership` uses the [copy replacement lifecycle and recovery contract](file-copy.md#mutation-and-recovery), including its failure aftermath rather than the stronger link-replacement guarantee.
When the shared target is missing, the planner uses `create_copy` with the stale identity recorded as superseded state.
That create still has ordinary no-adoption create semantics, and only its verified success atomically removes the stale old Known identity while recording the new one.

The [file-link transition table](#transition-table) and [copy transition table](file-copy.md#planning) own ordinary create, noop, replace, relocate, remove, and forget-missing selection. Both effects block unmanaged and unsafe observations. Every selected action is scheduled through the combined DAG in [Execution Order](#execution-order).

## Directory-Link Effect

DirectoryLink uses the same planner boundary and combined-DAG scheduling contract. [Directory Links](directory-link.md#planning-transitions) owns its complete transition table, including contiguous relocation, same-effect identity handoff, and blocked mixed-effect handoff.

## Schema Gate

Before a normal lifecycle command observes a target, reconciles an active operation, produces a plan, rewrites state, or mutates a target, it validates every profile and state document in that command's read set against the [Current Profile Schema](profiles.md#current-profile-schema) and [Current State Schema](state-and-recovery.md#current-state-schema).
For non-dry-run apply, acquiring the exclusive lock may precede this gate, but the gate precedes recovery and every other mutable state-repository operation.

If any required document has an unsupported schema version or invalid schema shape, the command reports invalid input and performs none of those actions.
In particular, it does not recover a schema-4 active operation, inspect a target that matches a schema-4 Known effect, or create a replacement state document. The explicit [Migration](migration.md) authoring operation is outside this lifecycle and is the only schema-4 conversion path.
An absent state file is the only state input that the current contract treats as an empty Known state.

Inspection commands apply the same gate only to documents their read-only contract names.
They remain prohibited from acquiring the apply lock, recovery, state rewrite, and target mutation.
