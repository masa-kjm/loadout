# Future Considerations

This directory records design work that is explicitly outside v0.2.0.
It is not a specification, implementation plan, compatibility promise, or user-facing command reference.

Future documents may identify risks, prerequisites, and candidate constraints.
They must not weaken or reinterpret the published v0.2.0 architecture or specifications.
Before a future capability is implemented, its final behavior must move into architecture and specification documents with test evidence.

## Topics

- [Task Resources](task-resource.md) considers controlled lifecycle tasks without treating arbitrary commands as safely idempotent.
- [File Copy and Directory Resources](file-copy-and-directory.md) considers content ownership and tree safety beyond symbolic links.
- [Remote Stores](remote-stores.md) considers reproducible, cache-backed source acquisition and supply-chain boundaries.
- [Profile Parameters](profile-parameters.md) considers reusable profiles with explicit, typed inputs and stable instance identities.
- [Resource Import](resource-import.md) considers the deliberately strong operation of importing an unmanaged target into a source store.
- [Inspection and Authoring Commands](inspection-and-authoring.md) considers read-only discovery, drift inspection, environment setup, and explicit configuration editing.
- [Resource Dependencies and Ordering](resource-dependencies-and-ordering.md) considers explicit ordering constraints without overloading declaration order or resource IDs.
- [Stages and Execution Ordering](stages-and-execution-ordering.md) considers coarse execution barriers that expand into a resource action graph without changing resource effects.
- [Schema Evolution and Migration](schema-evolution-and-migration.md) defines the future policy for strict schema versioning and safe, explicit data migration.

## Promotion Criteria

A future topic may be proposed for implementation only after its design answers:

1. What are the resolved desired, Known, and Actual representations?
2. What ownership evidence is required before every destructive action?
3. What is the no-mutation outcome for conflict, dry run, preflight failure, and interrupted execution?
4. Which lifecycle and state changes are necessary without bypassing existing boundaries?
5. Which filesystem, durability, executor, CLI, and platform tests prove the new behavior?
