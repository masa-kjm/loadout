# Migration Specification

## Scope and Generations

This specification defines explicit migration of managed state and profile documents through schema generations 3, 4, 5, and 6.
Migration is an authoring operation, not lifecycle execution, inspection, recovery, or ownership adoption.
It changes no resource target, source store, materialization, resource ID, effect, or durable ownership fact.
Normal readers remain schema-6-only and never migrate implicitly.

State and profiles share a managed-document generation: their individual `schema_version` fields identify the same generation, and the current generation is 6.
Each document retains its version because publication across documents is not atomic; migration creates no persistent global-version marker.
Runtime configuration schema 1 and environment configuration schema 2 are separate contracts and are not changed by migration or targeted by `--to-schema`.
The selected environment configuration MUST be a complete valid schema-2 document before a migration is presented.
A shared generation number alone does not establish state/profile compatibility; the checks below do.

## Isolation and Routes

`src/migration/` exclusively owns legacy DTOs, generation-specific parsing and validation, conversion, complete-inventory planning, compatibility checks, and migration-only preflight.
Normal declaration readers, state decoders, resolver, planner, executor, and inspection code MUST NOT import a legacy DTO or invoke conversion.
Outside that module, the sole migration edge is a crate-private facade accepting a request and returning data-only presentation or outcome values.
The command adapter obtains the presentation from the facade, then invokes facade execution once after confirmation or `--yes` acceptance and renders its result.
It MUST NOT discover or compare an inventory, decide eligibility or ordering, inspect an active operation, acquire a state lock, construct a candidate, or publish a document.
Facade values MUST NOT expose a legacy DTO, parsed document, inventory iterator, conversion candidate, lock or transaction guard, path handle, writer, or publication capability.

The facade owns route registration, whole-inventory planning, confirmed-snapshot comparison, and state-before-profiles orchestration.
Separate internal state and profile migration responsibilities own their respective validation, immediate rechecks, publication, and resumption rules.
Each adjacent generation has a private named route for both document kinds, even when one kind needs only validation and a version change.
Generation 3→4, 4→5, and 5→6 routes are registered; routes may be composed only in ascending adjacent order.
Missing, duplicate, descending, or unregistered edges fail before publication.
Adding a later generation requires explicit routes for both document kinds and registration; retiring a route removes its registration and private unit without expanding normal readers.
No command adapter or non-migration module may select or import a route-private item.

## Command and Presentation

```text
loadout migrate --config <path> [--to-schema <N>] [--yes]
```

Omitting `--to-schema` selects the binary's latest managed-document generation, 6 for this contract.
Explicit targets 4, 5, and 6 are supported; the number is a schema generation, not a release version.
Other targets and incomplete routes are rejected without a write.
`migrate state` and `migrate profiles` are not public commands; either old form is an argument error that points to `loadout migrate` and changes no document.

Before confirmation, migration reports the selected configuration, every discovered state and profile document, each document's current and target generation, its adjacent routes or no-change status, the exact write set, and whether normal commands can read the completed inventory.
It states that resource IDs, effects, and ownership facts cannot change.
When target 4 or 5 leaves any document below schema 6, the presentation and completion output warn that normal schema-6-only commands cannot read that document.
For a 5→6 conversion that moves an old `stages.<name>.after` relation to a root and broadens the resources constrained by that relation, the pre-confirmation presentation identifies the root, phase relation, and newly covered ordering. It does not claim broadening when the converted relation has the same scope.
For example, if schema-5 child `base` ordered its local `late` after local `early`, but root `workstation` already has another child assigned to each of those phase names, moving `early → late` to `workstation` also orders those other children. The presentation names `workstation`, `early → late`, and those newly constrained resources before confirmation.
Without `--yes`, the command requests confirmation and writes nothing until accepted; non-interactive execution requires `--yes`.
`--yes` skips only confirmation, never validation, snapshot comparison, locking, or publication checks.
An inventory already at the target succeeds without a write after validation and confirmed-snapshot comparison.

## Eligibility and Compatibility

Migration discovers every profile and validates each complete document with its recorded generation's migration-only validator.
It validates the optional state with its recorded generation's validator, including all internal identity, effect, hash, ordering, and ownership invariants, and requires the active operation to be absent.
An absent state remains absent; migration does not create a state file or acquire a state lock for it.
Malformed, unavailable, unsafe, unsupported, or unknown documents, an active or uncertain operation, an unrepresentable conversion, or a document newer than the target reject the entire plan before a write.
Migration never downgrades or skips a generation.

Each state route MUST preserve every validated resource ID, effect, definition hash, and ownership fact exactly.
The 3→4 state route is lossless only when the retained default instance name equals the defining profile ID; it does not recalculate an ownership hash or invent an instance mapping.
The 4→5 route initializes `applied_lifecycle_topology.current` and `.historical` to empty arrays and invents no ordering facts.
Every intermediate and final state candidate MUST pass its generation's complete validator.
The 5→6 state route requires `active_operation: null` and both `applied_lifecycle_topology.current` and `.historical` empty. It preserves every Known ID, effect, definition hash, and ownership fact without inventing phase membership or direct edges. This check applies to any route passing through 5→6, including 3/4→6. A state already at schema 6 may retain valid recorded topology; the empty-topology condition is not reapplied.

The 3→4 profile route changes only the root version and adds no parameters, bindings, aliases, resources, or instance mappings. The 4→5 route changes only the root version and adds no stages, dependencies, resources, aliases, bindings, or topology.
The 5→6 profile route may relocate schema-5 ordering declarations only when their placement and normalized meaning are unique in every applicable selected-root context. It must prove the following against the complete discovered inventory in memory:

- Each old child phase name maps to one root-owned phase with identical `after` relations across every declaration merged under that name; differing relations cannot be silently unioned. Every qualified old `after` reference must resolve to the same root phase relation in every applicable root context.
- Each old resource phase and dependency retains a unique effective meaning after conversion. A cross-instance `depends_on` moves to the unique parent instance whose include closure contains both resolved endpoints; absence or multiplicity of such a parent rejects conversion. Same-instance prerequisites remain local.
- Every placement remains valid under all applicable root selections and aliases; a conversion cannot use an include traversal order to choose a meaning. A declaration that needs a parent assignment must not be treated as an independently valid root.

If the route cannot prove uniqueness, it rejects before publication with the profile ID, declaration or reference, affected root contexts, and reason requiring manual correction. It never picks one context, silently unions phase relations, or rewrites resource identity or ownership.
Intermediate and final documents MUST pass complete generation-specific validation.
For a profile newer than an existing state, migration additionally proves compatibility with every intervening older generation: replace only the root version value one generation at a time, validate each resulting complete older profile, then apply the registered forward routes and verify that each intermediate is valid and the final bytes exactly reproduce the original.
The reverse check for 5→3 therefore includes both 5→4 and 4→3.
Version replacement for this proof does not change any other field, value, comment, order, or notation.
No publication provenance is required: any document satisfying these observable checks is eligible.
Profiles at or below the state generation need only valid forward conversions to the target.
Across the mixed inventory, migration also rejects duplicate profile IDs, missing references, cycles, and an invalid completed target-generation inventory. For target 6 it first constructs the entire target inventory in memory, then completely resolves every parameterless candidate that can satisfy its own root phase requirements. It rejects an invalid default root, any candidate whose full resolution fails, and any non-root profile not covered by at least one valid candidate closure. It reports uncovered profiles and invalid candidates with their IDs and resolution cause. Document-only parsing is not this inventory proof.
An existing Known definition need not equal the current profile declaration; ordinary lifecycle rules handle that difference after migration.

| Initial state | Profiles | Explicit target 5 outcome |
| --- | --- | --- |
| Absent | Any supported mix of 3, 4, and 5 | Keep state absent; advance only older profiles. |
| 3 | Any supported mix of 3, 4, and 5 | Prove newer-profile compatibility with 3; publish state 5 before older profiles. |
| 4 | Any supported mix of 3, 4, and 5 | Prove newer-profile compatibility with 4; publish state 5 before older profiles. |
| 5 | Any supported mix of 3, 4, and 5 | Retain state; advance only older profiles. |

Target 4 follows the same rules for documents no newer than 4; any state or profile newer than 4 rejects the whole plan before a write. Target 5 likewise rejects any schema-6 document before a write.
For target 6, absent state with profiles 3–6 keeps state absent and validates the entire completed inventory. Schema-5 state with profiles 3–6 requires empty current and historical topology before state-first publication. A schema-6 profile newer than schema-5 state must pass the intervening reverse compatibility proof; `includes[].stage` cannot pass by being ignored. Schema-6 state with profiles 3–6 retains valid schema-6 topology and advances only older profiles. Every state route through 5→6 applies the same empty-topology gate. Reverse compatibility checks still apply whenever a profile is newer than state.
Every intermediate state and profile candidate is validated in memory, and only each completed target-generation document is published once.

## Confirmed Snapshot and Execution

The presentation is bound to a snapshot containing the selected environment configuration's normalized path and exact bytes; profile-discovery directory paths, required safe path associations, and complete entry sets; every discovered profile's path, regular-file kind, and exact bytes; the state path and either its absence or exact bytes; and each document's selected route and write decision.
The facade MUST NOT choose new routes or add write targets after confirmation.
After confirmation and before the first lock acquisition or publication, the facade re-discovers and revalidates the configuration, full profile inventory, and state against that snapshot, including paths, associations, entry sets, kinds, contents, and state absence.
This comparison applies when state is absent, already at target, or needs migration.
Any difference fails before the first write.
If state needs migration, the state responsibility then acquires the existing exclusive state lock, repeats the same whole-snapshot comparison under the lock, and publishes only the fully validated target-generation state.
Lock contention or changed input prevents its publication.
Migration does not invoke recovery.

After successful state publication, the facade updates only that document's expected snapshot entry and confirms the target-generation state barrier before any profile publication.
Before each profile publication, it rechecks the configuration, discovery paths and complete entry sets, every profile, and the state against the updated expected snapshot.
After each successful profile publication it updates only that profile's expected entry.
An external addition, removal, kind change, path change, or byte change stops remaining publications; only the original confirmed write set may be published.
The state lock does not exclude external profile writers.
These comparisons do not promise detection of substitutions after the last check.

## Publication, Failure, and Resumption

For each state or profile, migration creates a unique temporary sibling, writes the complete target-generation candidate, flushes it where supported, reopens it, validates it with the target-generation migration validator, and atomically replaces the original on the same filesystem.
State publication follows the state repository's durability rules through a narrow migration-only publication interface; target 4 or 5 does not expand the normal schema-6 state reader.
No delete-then-create fallback, backup, or replacement of an unvalidated original is permitted.
Before profile publication, migration MUST no-follow inspect the final profile and required ancestors, prove their declared-path associations, and capture the exact validated regular-file source precondition.
It creates the temporary only under the proven safe parent, then no-follow verifies the expected regular-file temporary and its required path association.
Immediately before replacement it repeats the no-follow final and ancestor checks, source-byte comparison, and temporary-candidate check.
It MUST NOT replace after an unprovable or failed check.
The replacement primitive MUST retain the prior final file on replacement failure; an unsupported primitive fails before temporary creation or profile change.
After a replacement attempt, migration MUST no-follow reopen the final path, prove its required path association, and validate the expected complete target-generation document before reporting success.
If publication or post-publication verification fails or is unprovable, it preserves the observed final entry and may clean up only its exact temporary after a fresh no-follow proof of the recorded entry and safe parent.
It MUST NOT remove an unexpected or unprovable temporary, scan siblings, or remove a parent.
Failed cleanup or unprovable final observation is a publication failure, not proof that the original was retained.
The same post-publication success requirement applies before updating the confirmed snapshot.
An interruption after state publication or after some profiles leaves those verified publications in place without rollback.
A later explicit `loadout migrate` rebuilds and validates the whole mixed inventory, proves compatibility again, and advances only documents still below the requested target.
Unverified temporaries and external entries are not deleted as a recovery shortcut.
Normal readers continue to reject every old document they read until all required documents reach schema 6.
Migration does not promise a multi-document transaction, atomic identity comparison, uninterrupted path containment, or detection of every external substitution after its last recheck.
