---
name: test-design-loadout-v0-2
description: Design Loadout v0.2 test evidence from authoritative contracts. Use when planning tests for lifecycle behavior, filesystem safety, state durability, recovery, CLI acceptance, or Unix and Windows conformance.
---

# Design Loadout v0.2 Tests

Design evidence from contracts, not from the current implementation shape. Do not change code or tests unless the task explicitly requests implementation.

## Establish the contract

1. Read `docs/README.md`, then the architecture, specification, and testing documents that own the requested behavior. Architecture and specifications are authoritative; `future/` and draft material do not define v0.2 behavior.
2. Identify the observable outcome, owner, preconditions, post-condition, zero-mutation requirements, durable-state effect, failure aftermath, recovery behavior, and platform assumptions that apply.
3. Select the narrowest test layer that proves the contract. Add executor integration or CLI acceptance evidence when behavior is observable through the lifecycle or command interface.
4. Treat legacy tests and fixtures as historical material. Do not derive v0.2 expectations from them.

## Read the relevant design lenses

- For every plan, read [test layers](references/test-layers.md).
- For a filesystem mutation, ownership check, state commit, recovery path, schema rejection, or dry run, read [mutation and recovery](references/mutation-and-recovery.md).
- For symbolic-link, replacement, locking, reparse-point, or platform capability behavior, read [platform conformance](references/platform-conformance.md).

The references help choose evidence. They do not replace the authoritative documents they link to.

## Produce a test design

For each proposed test group, state:

- the owning contract and the precise behavior being proved;
- the test layer and why a narrower layer would be insufficient;
- the controlled setup, including only disposable home, store, configuration, and state directories;
- the action or failure injection point;
- assertions on Plan, diagnostics category, filesystem observation, Known state, operation record, process status, or output category as applicable; and
- the relevant negative, zero-mutation, recovery, and platform cases.

Prefer assertions on structured outcomes and filesystem or state snapshots over incidental diagnostic wording. Do not expose a new public API merely to make a test compile. State any unavailable platform capability or fault-injection limitation explicitly instead of silently omitting the evidence.

Use this response shape unless the user requests another format:

```markdown
## Contract coverage

| Contract | Test layer | Evidence |
| --- | --- | --- |
| ... | ... | ... |

## Cases

### <behavior group>

- Setup:
- Stimulus or injected failure:
- Assertions:

## Boundaries and limitations

- <Platform or testability limitation, if any.>
```
