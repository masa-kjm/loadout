# Future Considerations

This directory records design work that is explicitly outside the current v0.8.0 contract.
It is not a specification, implementation plan, compatibility promise, or user-facing command reference.

Future documents may identify risks, prerequisites, and candidate constraints.
They must not weaken or reinterpret the current architecture or specifications.
Before a future capability is implemented, its final behavior must move into architecture and specification documents with test evidence.

## Topics

- [Task Resources](task-resource.md) considers controlled lifecycle tasks without treating arbitrary commands as safely idempotent.
- [Directory Resources](file-copy-and-directory.md) considers directory copy and tree safety beyond the current DirectoryLink effect.
- [Remote Stores](remote-stores.md) considers reproducible, cache-backed source acquisition and supply-chain boundaries.
- [Resource Import](resource-import.md) considers the deliberately strong operation of importing an unmanaged target into a source store.

## Promotion Criteria

A future topic may be proposed for implementation only after its design answers:

1. What are the resolved desired, Known, and Actual representations?
2. What ownership evidence is required before every destructive action?
3. What is the no-mutation outcome for conflict, dry run, preflight failure, and interrupted execution?
4. Which lifecycle and state changes are necessary without bypassing existing boundaries?
5. Which filesystem, durability, executor, CLI, and platform tests prove the new behavior?
