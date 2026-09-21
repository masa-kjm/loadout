# Inspection Specification

## Scope

This specification defines the v0.5.0 read-only inspection commands: `status`, `profile list`, `profile show`, `resource list`, and `resource show`.
It extends the v0.3.0 `diff` inspection contract without changing that command.

Inspection reports facts from the declaration, Resolved Desired, Known, and Actual layers.
They are not Plans, do not select a repair, and do not change lifecycle ownership rules.
The [CLI](cli.md) specification owns shared help, configuration-selection, output, and exit-status rules.

## Read-Only Invariants

Every command in this specification MUST leave the target tree, source store, portable configuration, runtime configuration, state directory, and operation record unchanged.
It MUST NOT:

- acquire the exclusive apply lock;
- recover, close, amend, delete, or write an active operation;
- create a state directory, lock file, operation record, temporary entry, or target parent;
- invoke the planner, preflight, executor, or authoring workflow; or
- infer ownership, adoption, or an executable action from a declaration, target path, source path, or matching link.

Commands MUST use the same configuration discovery, declaration parsing, path resolution, state validation, and no-follow filesystem observation contracts as the lifecycle.
They MUST NOT parse raw YAML, normalize a path, or inspect a target through a separate presentation-only implementation.

## Command Surface

```text
loadout status [--config <path>] [<profile-id>]

loadout profile list [--config <path>]
loadout profile show [--config <path>] <profile-id>

loadout resource list [--config <path>] [<profile-id>]
loadout resource show [--config <path>] [<profile-id>] <resource-id>
loadout resource list --known
loadout resource show --known <resource-id>
```

`--config` selects the portable environment configuration under the rules in [Configuration](configuration.md).
For `status` and Desired-resource commands, the optional `<profile-id>` selects one discovered root profile; when it is absent, `default_profile` is REQUIRED.
`--known` conflicts with `--config` and a positional profile ID.

`profile show` and desired `resource show` require an exact ID.
Profile IDs and fully qualified resource IDs use the grammar and identity rules in [Profiles](profiles.md).
An unknown requested ID is an input error.

## Profile Inspection

### `loadout profile list`

`profile list` reads the selected portable environment configuration and discovers profile declarations through its configured discovery paths.
It reports every discovered profile ID in lexicographic order.
It does not read state, an active operation, a resource target, or a store source.

Profile discovery and each profile document MUST satisfy schema parsing and identifier validation.
The command does not resolve includes, bind stores, inspect source entries, select a root profile, or inspect a resource target.
It therefore reports discoverable declarations, not a validated Resolved Desired set.

### `loadout profile show`

`profile show` performs the same configuration and profile discovery as `profile list`, then reports the selected profile's declared ID, includes, and resource declarations.
Its resource listing is ordered lexicographically by resource ID; include order is presented in the declaration order because that order is part of profile-composition semantics.

The command does not resolve the include closure, bind stores, inspect source entries, read state, inspect a resource target, or report an active operation.
A displayed declaration does not establish that it can resolve or apply.

## Resource Inspection

### Desired Resources

Without `--known`, `resource list` and `resource show` resolve one selected root profile according to [Profiles](profiles.md).
They report only that root's Resolved Desired resources.
They MUST perform all declaration, include, store binding, path normalization, collision, and semantic validation required to produce Resolved Desired.
They MUST NOT inspect a resource target, read state, or report an active operation.
For a copy resource, they compute the source-content fingerprint required by its Resolved Desired representation; this source read does not authorize mutation or establish target ownership.
They do not invoke any other source check that is required only for a later filesystem mutation.

`resource list` reports resources in lexicographic fully qualified resource-ID order.
`resource show` reports one exact fully qualified resource ID from the selected Resolved Desired set.
The output identifies the root profile and the resource's fully qualified identity, type, resolved source path, resolved target path, and operation.

### Known Resources

With `--known`, `resource list` and `resource show` read only the validated state repository.
They do not select or read portable configuration, a profile, a store, a source entry, or a target.
They validate the complete state document, including an active operation when present, but do not report it or make a decision from it.
`resource list --known` reports Known resources in lexicographic fully qualified resource-ID order.
`resource show --known` reports one exact Known resource identity and its recorded type-specific ownership facts.

An absent state file is an empty Known set, as specified in [State and Recovery](state-and-recovery.md).
An unsupported, malformed, unreadable, or invariant-violating state document is an error; the command MUST NOT rewrite, migrate, or otherwise repair it.
An unknown Known resource ID is an input error.

## Status

`status` reports one selected root profile's Desired state, all Known state, and the corresponding Actual observations.
It reports any active operation from the validated state document, including every action whose status is `pending`, `running`, or `uncertain`.
Displaying an active operation does not recover or modify it.

The command reads state before target observation.
If state cannot be read and validated, `status` MUST fail without inspecting a target.
If state is valid but selected-declaration loading or resolution fails, `status` MUST report the active operation when one exists, report that Desired state is unavailable, return the applicable failure status, and MUST NOT inspect a target.

When both Desired and Known state are available, `status` compares the union of their fully qualified resource IDs in lexicographic order.
For every Known resource it performs the effect-specific no-follow target and parent observation required by [File Links](file-link.md) or [File Copies](file-copy.md), using that record's ownership evidence.
For every Desired-only resource it performs the corresponding observation using the resolved desired effect.
An observation is informational only and does not prove ownership for a Desired-only resource.

For an ID that exists in both sets, `status` MUST distinguish equal and different definitions using the canonical `definition_hash` and the typed resolved fields that it represents.
Equal IDs with different definitions are a reported definition difference, not a matched managed resource and not an instruction to replace, relocate, remove, or hand off ownership.
Target-path equality alone does not associate two records.

The report MUST label the comparison that supports each result.
It MUST NOT collapse Desired-to-Known, Known-to-Actual, and Desired-to-Actual facts into an unqualified `in sync` result.
At minimum, it reports these categories:

| Category | Required facts |
| --- | --- |
| `desired_only` | a Desired identity with no Known identity |
| `known_only` | a Known identity with no selected Desired identity |
| `definition_changed` | the same identity exists in both sets but their definitions differ |
| `recorded_and_expected` | equal Desired and Known definitions plus the expected Known-to-Actual link observation |
| `drifted` | a Known resource with missing, different-link, other-entry, unsafe-parent, or unavailable Actual observation |
| `desired_target_observation` | Actual observation of a Desired-only resource, explicitly labeled as non-ownership evidence |
| `active_operation` | a persisted operation and unfinished action statuses |
| `desired_unavailable` | valid state but an unavailable or invalid selected declaration before target observation |

The exact human-readable wording and layout are not a machine-readable contract.
Every displayed category MUST retain enough identity, path, observation, or diagnostic context for an operator to identify its subject.

## Failure and Exit Status

Observed drift, a Desired-only resource, a Known-only resource, a definition difference, and an active operation are reportable facts, not an inspection runtime failure.
When all required reads and observations complete, inspection commands return status `0` even when the report requires operator attention.

Invalid arguments, an unknown requested ID, invalid declaration input, unsupported configuration or profile schema, declaration-resolution failure, or an unavailable required default profile return status `2`.
Unreadable state or configuration files, state decoding failures, and unrecoverable filesystem-observation failures return status `1`.
The command MUST perform no mutation for either outcome.

An unsafe parent or unexpected target entry that the no-follow inspector can classify is a reported observation, not an I/O failure.
If an observation cannot be established because of an I/O failure, the report identifies that resource as unavailable and returns status `1` after reporting any independently completed observations.

## Non-Goals

v0.5.0 inspection does not provide configuration editing, state repair, operation recovery, migration, import, target adoption, forceful takeover, a repair form of `diff`, a saved plan, or machine-readable output.
