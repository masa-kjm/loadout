# Directory Resources

## Status

This is a non-binding design note for directory-resource work beyond the current DirectoryLink operation.
Regular-file copy is specified in [File Copies](../specs/file-copy.md), and directory symbolic links are specified in [Directory Links](../specs/directory-link.md); neither is restated here.

## Why They Are Separate

Directory copy and synchronization have a larger destructive surface than current file effects and must not be added as a small variation of file copy or DirectoryLink.

## Directory Questions

Future tree materialization requires an explicit choice among incompatible models:

- a one-way copy that never removes destination entries;
- a synchronized tree with defined deletion semantics; or
- a managed manifest of individual files.

Each model has different ownership and recovery properties.
A directory copy must answer whether a manually created descendant blocks removal, whether a deleted source file deletes a target file, how empty directories are treated, and whether a tree is fingerprinted as a whole or per entry.

The future design must account for nested symlinks, junctions, reparse points, case-insensitive paths, executable bits, ACLs, partial traversal failures, and target files that are not owned by Loadout.

## Safety Baseline

Any future directory design must preserve the current [architecture boundaries](../architecture/boundaries.md), [lifecycle safety rules](../specs/lifecycle.md), and [external filesystem concurrency contract](../specs/file-link.md#external-filesystem-concurrency).

## Remaining Directory Promotion Work

Promotion requires a separate specification for every supported directory model.
Each needs a state schema, complete transition table, platform failure guarantees, and tests for nested unsafe entries, partial traversal failure, replacement failure, and recovery after interruption.
