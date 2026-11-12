---
name: test-design-loadout-v0-2
description: Design Loadout test evidence from current contracts. Use for lifecycle, filesystem, state, recovery, CLI, or platform test planning.
---

# Design Loadout Tests

Design evidence from contracts, not from the current implementation shape. Do not change code or tests unless the task explicitly requests implementation.

## Establish the contract

1. Read `docs/README.md`, the owning architecture and specifications, and `docs/development/testing.md`. Use the current contract rather than historical sections or legacy fixtures.
2. Identify the outcome, preconditions, post-condition, failure aftermath, recovery, and platform facts that apply.
3. Select the narrowest test layer that proves the contract, adding integration or CLI evidence for observable behavior.

## Read the relevant design lenses

- For every plan, read [test layers](references/test-layers.md).
- For a filesystem mutation, ownership check, state commit, recovery path, schema rejection, or dry run, read [mutation and recovery](references/mutation-and-recovery.md).
- For symbolic-link, replacement, locking, reparse-point, or platform capability behavior, read [platform conformance](references/platform-conformance.md).

The references help choose evidence. They do not replace the authoritative documents they link to.

## Produce a test design

For each test group, give its owning contract, layer, disposable setup, stimulus or failure boundary, observable assertions, and applicable negative, zero-mutation, recovery, and platform cases. Prefer structured outcomes and filesystem or state snapshots over incidental wording or private implementation seams. Do not add a public API solely for a test. Report platform or testability limitations.
