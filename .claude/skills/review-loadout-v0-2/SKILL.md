---
name: review-loadout-v0-2
description: Review Loadout changes and pull requests against current contracts, architecture boundaries, and test evidence. Use for behavior, safety, schema, state, or CLI reviews.
---

# Review Loadout Changes

Review evidence, not intent. Do not edit the reviewed implementation unless the task explicitly requests changes.

## Establish the review surface

1. Read `CONTRIBUTING.md`, `docs/README.md`, and the architecture, specifications, and testing guidance that own the change.
2. Identify the base and inspect the complete diff. For a pull request, inspect its body, linked issue, comments and reviews, changed files, and CI read-only; invoke `gh` through `bash -lic` as required by the repository instructions.
3. Map explicit acceptance criteria and applicable contract requirements to implementation and test evidence. Report unresolved contract ambiguity instead of inferring behavior from implementation or drafts.

## Use the relevant review lenses

- For architecture or implementation boundary changes, read [layer boundaries](references/layer-boundaries.md).
- For filesystem writes, persistent state, ownership, recovery, path handling, schema versions, or dry-run behavior, read [mutation safety](references/mutation-safety.md).
- For test coverage judgments, read [testing evidence](references/testing-evidence.md).

Read only relevant lenses; they guide evidence collection and do not replace the contracts.

## Validate and report

Run focused tests and the relevant locked checks when the environment permits. State precisely which commands ran, their results, and why any others could not run.

Report evidence-backed findings by severity with affected locations, acceptance coverage, and validation results. State explicitly when no finding is supported. Do not treat a style preference as a contract violation.
