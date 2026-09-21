# File Copy and Directory Resources

## Status

This is a non-binding design note.
Regular-file copy is specified for v0.5.0 in [File Copies](../specs/file-copy.md).
This note remains non-binding and concerns only later directory-resource work.

## Why They Are Separate

A file link exposes a source path directly and has a narrow ownership proof: the target must remain the exact link that Loadout created.
A copied file and a directory tree need content-based ownership and drift rules instead.
They must not inherit file-link behavior by implication.

Directory materialization remains independent from the promoted regular-file copy.
Directory behavior has a larger destructive surface and must not be added as a small variation of file copy.

## Promoted File-Copy Direction

The v0.5 specification owns file-copy Known state, content fingerprints, removal, replacement, and recovery.
This note must not be used to reinterpret that contract.

Candidate outcomes include:

- source changed while the target still matches the applied fingerprint: a replace may be proposed;
- target changed while the source has not: block and require an explicit user decision outside the normal apply path;
- both source and target changed: block as a conflict;
- target missing: create when desired, or forget the Known-state record when stale; and
- unexpected target kind or parent: block before mutation.

Replacement should write new content to a safe temporary file in the target parent and use a platform-specific replacement primitive only when it preserves the documented failure aftermath.
It must not delete the old file before the new content is ready.

The final schema must decide exactly which metadata is part of the ownership fingerprint, including executable permissions, line-ending normalization, timestamps, ACLs, and platform-specific attributes.

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

Any copy or directory design must preserve the v0.2.0 boundaries:

- source and target containment must be proven physically, not lexically;
- unexpected entries must not be followed, replaced, or removed;
- a target is removed only with both Known ownership evidence and matching Actual state;
- dry run, validation, conflicts, and failed preflight make no filesystem mutation;
- Known state changes only after post-condition verification; and
- recovery never deletes user-visible artifacts to clean up an uncertain operation.

## Remaining Directory Promotion Work

Promotion requires a separate specification for every supported directory model.
Each needs a state schema, complete transition table, platform failure guarantees, and tests for nested unsafe entries, partial traversal failure, replacement failure, and recovery after interruption.
