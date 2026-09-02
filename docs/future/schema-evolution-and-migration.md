# Schema Evolution and Migration

## Status

This is a non-binding design note.
v0.2.0 implements only version-1 schemas and provides no migration command.
This document defines the required direction before a future release changes any portable or machine-local schema.

## Principle

A schema version identifies both structure and semantics.
A command MUST NOT treat a document with another schema version as if it had the semantics of the version it implements.
In particular, it must not ignore unknown ordering, ownership, recovery, or resource-lifecycle data and continue to plan or apply.

Every independently persisted Loadout document needs an explicit schema version at its top level.
This includes runtime configuration, portable environment configuration, profile files, durable state, operation records when they are independently persisted, and future cache or resource-specific control data.
Versions are local to their document type; a profile version is not a state version.

## Version Format

A schema version is a positive, monotonically increasing integer.

```yaml
schema_version: 1
```

The next incompatible schema contract for the same document type is `2`, then `3`, and so on.
Schema versions do not use semantic-version triples or dates: they describe a document contract, not a release or publication date.
A code-only change that preserves the complete persisted contract does not advance the schema version.

## Version Mismatch

When a lifecycle or inspection command encounters an unsupported schema version, it MUST stop before it inspects a managed target or changes persistent state.
It reports the document type, location, encountered version, and required migration direction.
`plan`, `apply`, `validate`, and `diff` never perform an implicit migration.

A newer implementation may contain readers for older schema versions solely to support migration.
It must not use such a reader to execute the normal lifecycle against an older document.
An older implementation encountering a newer document must also fail safely; it must not ignore newer fields or preserve them while applying an older lifecycle.

## Explicit Migration

Migration is a separate, explicit authoring operation.
Its final command syntax is undecided, but it must identify the exact documents it will change, their source and target schema versions, and the resulting semantic changes before it writes anything.

A migration performs this sequence for each document it changes:

1. Parse and validate the source document according to its declared source schema.
2. Produce the target-schema document without changing managed targets, stores, or resource materializations.
3. Validate the complete target document according to the target schema.
4. Present the changed files and semantic effects for review and obtain the required confirmation.
5. Write through a unique temporary file in the same directory, flush it, re-open and validate it, then atomically replace the original file.

Migration must never reinterpret a resource identity merely to fit a new schema.
It preserves fully qualified resource identity, ownership evidence, and effect semantics whenever a lossless conversion exists.
If a conversion would change identity, target, source, removal behavior, or resource effect, migration must stop and require an explicit user mapping or a separately specified ownership-transfer workflow.

## State Migration

State migration is more constrained because it can affect recovery and future destructive-action safety.
It MUST hold the exclusive state lock and is permitted only when `active_operation == null`.
It MUST NOT recover, close, or reinterpret an operation recorded under an older state contract.
If `active_operation` is non-null, including an uncertain operation, migration MUST reject without writing state.
The operator must use the binary that implements the old state contract to complete recovery and close the operation before migration.

The state migration writes a complete target state atomically using the same durability protocol as state commits.
It MUST NOT mutate a target, source store, profile, or portable environment configuration as part of state migration.
If it cannot validate the migrated state before replacement, the old state remains authoritative.

When a new feature adds durable metadata, such as Applied Lifecycle Topology, migration may initialize a conservative empty or legacy representation.
The feature must define the behavior of resources without historical metadata; it must not invent ownership or ordering facts that were not recorded previously.

## Multi-Document Migration

Portable configuration, profiles, and local state may reside in different directories and cannot be claimed to update atomically as one filesystem transaction.
The final migration design must either migrate one independently valid document at a time or use a durable migration operation record that makes an interrupted multi-document migration visible and recoverable.
It must not leave a partly converted collection that a normal lifecycle command silently accepts.

## Compatibility Scope

The goal is not indefinite execution compatibility between every binary and every schema version.
The goal is safe evolution:

- current lifecycle commands operate only on their exact supported schema versions;
- a migration-capable newer binary reads older versions only to convert them deliberately;
- existing resources keep their ownership and effect semantics across a lossless migration; and
- an incompatible change stops with a diagnostic rather than performing an implicit replacement, removal, or adoption.

For a single-user v0.2.x development period, this policy permits rapid schema evolution without adding speculative extension fields or silently weakening validation.

## Required Promotion Work

Before a schema-changing feature ships, its design must define source and target schemas, migration eligibility, identity-preservation rules, interruption behavior, confirmation and reporting, rollback or recovery boundaries, and tests for malformed source input, failed replacement, lock contention, active and uncertain operations, interrupted state migration, old-binary rejection, and zero target/store mutation during migration.
