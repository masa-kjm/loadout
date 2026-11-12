# Migration Specification

## Scope

This specification defines the explicit v0.7 schema-4 to v0.8 schema-5 migration commands.
Migration is an authoring operation, not a lifecycle, inspection, recovery, or ownership operation.
It changes neither a resource target, source store, resource materialization, nor Known ownership evidence.
It does not adopt an effect, invent an instance mapping, or perform an ownership handoff.

Only complete, valid, losslessly convertible schema-4 profile and state documents are supported sources.
Schema 3, unknown versions, malformed documents, invalid ordering, identity/effect/hash mismatches, active or uncertain operations, lock contention, and publication failures fail closed.

## Historical Context

The v0.7 schema-3-to-4 migration route is superseded and is not a v0.8 migration reader, command, route, or protocol.
It is retained here only as historical release context and grants no compatibility behavior.

## Isolation

`src/migration/` exclusively owns legacy schema-4 profile and state DTOs, source-schema parsing, inventory validation, lossless conversion, and migration-only preflight.
Normal declaration readers, normal state decoders, resolver, planner, executor, and inspection code MUST NOT import a legacy DTO or invoke conversion.
Outside `src/migration/`, the only migration edge is a crate-private facade that accepts a request and returns a presentation or outcome.
The command adapter may construct that request from parsed options, present facade-provided information, obtain confirmation, invoke the selected migration operation exactly once for a successful migration path, and render its structured result.
It MUST NOT parse a schema-4 document, discover or compare an inventory, decide state-first or active-operation eligibility, acquire a state lock, validate a conversion candidate, or publish a document.
The facade's presentation and outcome values are data-only reporting values.
They MUST NOT expose a legacy document or DTO, parsed YAML or JSON value, inventory iterator, conversion candidate, lock or transaction guard, path handle, writer, or publication capability.
The migration facade may use narrow current-schema validation and state-repository lock and publication interfaces.
Migration-specific parsing or conversion MUST NOT be added to a normal lifecycle path.

## Version-Pair Route

Every supported migration is a private, named source-to-target version-pair unit below `src/migration/`.
For v0.8, the sole supported unit is `schema_4_to_5`.
It owns the route's legacy DTOs, source parsing, validation, conversion, and route-specific tests.

The facade is the unit's sole stable edge and selects only explicitly registered supported routes.
No normal reader, command adapter, or other non-migration module may select, import, or depend on a route-private item.
An unregistered source-to-target pair fails as unsupported before route parsing, conversion, or publication.

Adding a future route requires a distinct private unit and an explicit facade registration.
Retiring a route removes its registration and private unit after its documented support window; it does not leave a compatibility alias, expand a normal reader, or expose an old route API.

## Command Contract

```text
loadout migrate state --config <path> [--yes]
loadout migrate profiles --config <path> [--yes]
```

Before any write, each command reports the exact affected documents, their source and target schema versions, the required next step, and that no resource ID, effect, or ownership change is permitted.
Without `--yes`, it requests confirmation and performs no write.
In a non-interactive session, `--yes` is required.

`migrate state` is the mandatory first operation when state exists.
`migrate profiles` is permitted only when state is absent or already schema 5.
The state-first barrier ensures that normal v0.8 readers cannot recover or mutate a target from schema-4 state before a schema-5 profile is published.

## State Migration Protocol

`migrate state` discovers every profile in the configured inventory and validates that the inventory is complete, available, valid, and entirely schema 4.
It validates the schema-4 state and requires its active operation to be absent; an active, uncertain, or failed-to-classify operation is rejected without target observation or mutation.
It then acquires the existing exclusive state lock, re-discovers and revalidates that complete profile inventory immediately before publication, and publishes schema-5 state only if both validations agree.
Migration does not invoke recovery.

Any changed, missing, malformed, unavailable, mixed-version, schema-3, or unknown profile inventory fails closed with the original state byte-for-byte unchanged.
The same result is required for an invalid state, an active or uncertain operation, lock contention, unrepresentable data, or an unsuccessful publication.

The converted state preserves every validated schema-4 resource ID, effect, ownership fact, and hash exactly.
It initializes `applied_lifecycle_topology.current` and `applied_lifecycle_topology.historical` to empty arrays and invents no ordering facts.

## Profile Migration Protocol

After state is absent or the schema-5 state barrier is present, `migrate profiles` discovers and completely validates its profile inventory before publishing any profile.
The accepted inventory contains only valid schema-4 profiles to convert and valid schema-5 profiles whose remaining content is valid schema-4 content.
Schema-3, unknown-version, malformed, unavailable, or otherwise invalid entries fail closed.
A schema-5 profile meets that observable condition only when replacing its `schema_version: 5` with `schema_version: 4` produces a complete valid schema-4 profile.
Migration records no provenance for profile publication: an externally authored or substituted schema-5 profile that meets this observable condition is accepted, and one that does not is rejected.
Only after that complete preflight succeeds does it convert every remaining schema-4 profile in the inventory.
Each accepted conversion changes only `schema_version: 4` to `schema_version: 5`.
It does not add stages, dependencies, resources, aliases, bindings, or topology.

Immediately before each individual publication, the command revalidates that profile's recorded source precondition and safe path; it does not need to revalidate the whole inventory between publications.
An interruption or failure after some publications leaves schema-5 state and mixed profiles.
This is intentionally fail closed: normal commands reject the mixed inventory, and a later explicit `migrate profiles` may convert only the remaining valid schema-4 profiles.
The protocol is not a multi-document transaction and makes no rollback claim.

## Publication and Concurrency

For each state or profile document, migration creates a unique temporary sibling, writes the complete candidate, flushes it where supported, reopens it, validates the schema-5 result, and atomically replaces the original on the same filesystem.
State publication also follows the existing state durability rules.
No delete-then-create fallback, backup, or replacement of an unvalidated original is permitted.

Before profile publication, migration MUST no-follow inspect the profile file and every required ancestor, prove their required declared-path associations, and capture the exact validated regular-file source precondition.
It creates the temporary sibling only under the proven safe parent, and, after creating it, MUST no-follow verify the temporary's expected regular-file candidate and required path association.
Immediately before atomic replacement, it MUST again no-follow inspect the final profile file and required ancestors, prove their associations, and verify the captured source precondition and temporary candidate.
It MUST NOT replace after a failed or unprovable recheck.
The replacement primitive MUST preserve the prior final file if replacement fails; if the platform cannot provide that guarantee, migration fails before creating a temporary or changing the profile.

After any replacement attempt, migration MUST no-follow reopen the final path, prove its required path association, and validate the expected complete schema-5 profile before reporting success.
If publication or post-publication verification fails or is unprovable, it preserves the observed final entry and may clean up only the exact temporary entry that a fresh no-follow check proves is still its expected candidate at the recorded path with a safe parent and required path association.
It MUST NOT remove an unexpected, unsafe, unavailable, or unprovable temporary entry, scan siblings, or remove a parent.
An unsuccessful cleanup or unprovable final observation is a publication failure, not evidence that the original profile was retained.
This protocol does not claim atomic identity comparison, uninterrupted containment, or detection of every external substitution: an entry substituted after the last recheck may be replaced where the platform primitive permits replacement.

Profile files are outside the state lock.
The final locked profile-inventory recheck prevents a pre-publication state migration from accepting a changed inventory, but does not make state and profile documents a cross-document transaction.
If a profile is substituted after that recheck, normal schema validation handles it as a fail-closed error; migration and lifecycle do not adopt it or mutate a target.

## Normal-Reader Boundary

Normal lifecycle, validation, dry-run, and inspection readers reject schema 4 and unknown versions before target observation, recovery, planning, state rewrite, or mutation.
No normal reader may use this route or any compatibility parser.
