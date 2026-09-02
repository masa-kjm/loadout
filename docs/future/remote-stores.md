# Remote Stores

## Status

This is a non-binding design note.
Remote stores are not part of v0.2.0 and this document defines no network behavior, authentication mechanism, or store schema.

## Problem

Portable profiles may eventually need source assets from a Git repository or another remote origin.
That adds source authenticity, revision selection, cache durability, offline behavior, and credential boundaries that do not exist for local stores.

Remote acquisition must remain outside the planner.
The planner should receive only a resolved local source representation after acquisition and validation have completed.

## Candidate Design Direction

A remote store could materialize into a Loadout-owned cache and expose a read-only local source root to resource resolution.
The resolved store identity would need to include at least the origin, immutable revision, cache location, and content or manifest fingerprint.

Floating revisions should not be the default.
A branch or tag would need to resolve to an immutable commit before a plan can claim reproducible source input.
The lock or state data would need to record that resolved revision and the verification evidence used for it.

## Link and Cache Lifetime

A file link to a remote-store cache becomes broken when the cache is pruned or replaced.
For that reason, a future remote-store design should not treat a cache-backed source as a valid target for the v0.2.0 file-link operation.
One possible later constraint is to allow remote stores only with a copy resource whose applied content is independently recorded.

This is a design direction, not a promise that remote stores will support copy or any other materialization mode.

## Security and Ownership Questions

The final design must answer:

- which source protocols and origin forms are supported;
- how credentials are supplied without entering portable configuration or durable state;
- what verification proves the checked-out content matches the requested immutable revision;
- who owns the cache, lock files, temporary downloads, and garbage collection;
- how concurrent updates, interrupted fetches, and cache corruption are recovered;
- whether offline use can rely on a verified cache and how that condition appears in a plan; and
- how a source update is separated from environment apply so planning remains deterministic.

Source acquisition and cache management must not write to a user-declared local store.
They must not make the planner perform network I/O or silently change a resolved revision during apply.

## Required Promotion Work

Promotion requires a remote-store schema, acquisition and lock protocol, cache-state model, failure-after-effects rules, supply-chain threat model, and platform-aware tests for interrupted downloads, invalid revisions, cache replacement, concurrent access, offline operation, and credential redaction.
