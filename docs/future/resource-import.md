# Resource Import

## Status

This is a non-binding design note.
Resource import is not part of v0.2.0 and this document defines no command syntax or mutation protocol.

## Problem

Import turns an existing, unmanaged target into source material and a Loadout declaration.
Unlike ordinary apply, it can modify a user-owned source store, authoring configuration, and the target being imported.
It is therefore a strong authoring operation, not a variant of `plan`, `apply`, or automatic drift repair.

For example, importing `~/.gitconfig` may copy its content into a selected local store, add a profile resource declaration, and replace the original target with the selected materialization.
Each of those effects has different ownership and recovery properties.

## Safety Baseline

An import must never infer consent from an unmanaged target or from an ordinary `apply` confirmation.
It requires an explicit import request naming the target and every destination that can be changed, including the store, profile, resource identity, and desired materialization mode.

Before mutation, an import design must:

1. inspect the source target without following unsafe paths;
2. resolve and physically validate the selected store and authoring files;
3. detect source-path, resource-ID, profile-ID, and target collisions;
4. capture the expected content and configuration preconditions;
5. show a complete, reviewable diff for store content, declaration edits, and target changes; and
6. obtain a confirmation specific to that exact import proposal.

The confirmation must not authorize a changed proposal.
Immediately before each mutation, the executor must recheck the recorded precondition and stop on a mismatch.
An import must not overwrite an existing store asset, profile file, or configuration entry merely because the target was selected by the user.

## Separation from Normal Lifecycle

Normal v0.2.0 planning observes local stores as read-only and never adopts unmanaged targets.
That remains true after import exists.
Import must use a distinct authoring workflow and must not add a planner action such as `adopt`, `force_replace`, or `import_target` to the normal convergence lifecycle.

Only after every required authoring mutation has completed and its recorded post-condition is verified may the imported resource enter Known state through the ordinary resource lifecycle.
If the target must change from an unmanaged regular file to a managed link or copy, that replacement needs a separately specified ownership-transfer rule; it must not inherit file-link removal authority from a newly written declaration.

## Durability and Recovery

An import can span multiple independently durable locations: a source store, profile/configuration files, a target path, and Loadout state.
It cannot assume a single atomic filesystem replacement covers the whole operation.

The final design needs a dedicated operation record that captures preconditions, staged artifacts, committed effects, and recovery states for every location.
Recovery must prefer leaving a visible but truthful incomplete import over deleting or overwriting user content to simulate rollback.
It must distinguish an uncommitted staged copy from an imported source asset that a user has since edited.

Import should use a safe staging area and atomic replacement only within the documented boundary of each individual file.
Cross-file and cross-directory consistency must be established through the operation record, verification, and explicit recovery diagnostics rather than an unsupported transaction claim.

## Content and Privacy Questions

The final design must decide which entry kinds are importable, how executable bits, ACLs, extended attributes, symlinks, and platform-specific metadata are handled, and whether binary files are previewed or represented by metadata only.
It also needs a policy for secrets: an import must not display sensitive content in a terminal, log it in diagnostics, or silently copy it to a less protected store.

## Required Promotion Work

Promotion requires a final command contract, explicit destination selection, a multi-location durability model, exact recovery rules, content and metadata rules, and tests for collision, confirmation, interrupted copies, configuration-write failure, target-replacement failure, stale preconditions, user edits during recovery, and Unix/Windows path safety.
