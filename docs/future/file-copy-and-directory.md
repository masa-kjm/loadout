# Directory Resources

## Status

This is a non-binding design note for later directory-resource work.
Regular-file copy is specified for v0.5.0 in [File Copies](../specs/file-copy.md) and is not restated here.

## Why They Are Separate

Directory materialization has a larger destructive surface than either current file effect and must not be added as a small variation of file copy or file link.

## Directory Questions

Directory resources require an explicit choice among incompatible models:

- a directory symbolic link;
- a one-way copy that never removes destination entries;
- a synchronized tree with defined deletion semantics; or
- a managed manifest of individual files.

Each model has different ownership and recovery properties.
A directory copy must answer whether a manually created descendant blocks removal, whether a deleted source file deletes a target file, how empty directories are treated, and whether a tree is fingerprinted as a whole or per entry.

The future design must account for nested symlinks, junctions, reparse points, case-insensitive paths, executable bits, ACLs, partial traversal failures, and target files that are not owned by Loadout.

## Safety Baseline

Any directory design must preserve the v0.5.0 safety boundaries:

- source and target containment must be proven physically, not lexically;
- unexpected entries must not be followed, replaced, or removed;
- a target is removed only with both Known ownership evidence and matching Actual state;
- dry run, validation, conflicts, and failed preflight make no filesystem mutation;
- Known state changes only after post-condition verification; and
- recovery never deletes user-visible artifacts to clean up an uncertain operation.

## Remaining Directory Promotion Work

Promotion requires a separate specification for every supported directory model.
Each needs a state schema, complete transition table, platform failure guarantees, and tests for nested unsafe entries, partial traversal failure, replacement failure, and recovery after interruption.
