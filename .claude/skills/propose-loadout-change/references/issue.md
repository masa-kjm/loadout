# Issue Proposals

Use an issue to define a user-visible behavior change or bug fix before implementation. The issue must let a reviewer judge correctness without knowing the proposed implementation.

Use this structure from `CONTRIBUTING.md`:

```markdown
## Problem

<Observed failure or required behavior and its impact.>

## Reproduction

1. <Smallest reliable step, when applicable.>

## Expected behavior

- <User-visible result and failure aftermath.>

## Acceptance criteria

- <Independently testable requirement.>
```

For a filesystem mutation, state the target and source scope, ownership proof, preconditions, post-condition, dry-run behavior, failure aftermath, recovery behavior, Unix and Windows expectations, and intentional non-goals.

Link the owning v0.2 architecture or specification document. Do not substitute a preferred implementation for a behavioral acceptance criterion.
Do not include a `Closes #N` reference in an issue proposal.
