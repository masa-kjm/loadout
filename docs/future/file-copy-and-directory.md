# File Copy and Directory Resources

## Status

This is a non-binding design note.
File copy and directory resources are not part of v0.2.0 and this document defines no additional resource schema.

## Why They Are Separate

A file link exposes a source path directly and has a narrow ownership proof: the target must remain the exact link that Loadout created.
A copied file and a directory tree need content-based ownership and drift rules instead.
They must not inherit file-link behavior by implication.

File copy and directory materialization should be promoted independently.
Directory behavior has a larger destructive surface and must not be added as a small variation of file copy.

## Candidate File-Copy Rules

A copied file would need Known state containing the applied content fingerprint, resolved source, resolved target, and relevant metadata contract.
Removal would be safe only when the current target content matches the applied fingerprint and its entry kind remains expected.

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

## Required Promotion Work

Promotion requires separate specifications for file copy and every supported directory model.
Each specification needs a state schema, a complete transition table, platform failure guarantees, and tests for content drift, target-kind changes, nested unsafe entries, partial-copy failure, replacement failure, and recovery after interruption.
