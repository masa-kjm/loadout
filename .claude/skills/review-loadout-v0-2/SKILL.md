---
name: review-loadout-v0-2
description: Review Loadout v0.2 changes and pull requests against authoritative contracts, architecture boundaries, safety invariants, and required test evidence. Use especially for filesystem mutation, ownership, recovery, paths, schema, state, or CLI changes.
---

# Review Loadout v0.2 Changes

Review evidence, not intent. Do not edit the reviewed implementation unless the task explicitly requests changes.

## Establish the review surface

1. Read `CONTRIBUTING.md`, then `docs/README.md`. Read the applicable architecture, specification, and testing documents before judging behavior.
2. For a pull request, inspect its issue, description, comments, changed files, and CI status before concluding. Use the repository's read-only GitHub workflow; invoke `gh` through `bash -lic` as required by `AGENTS.md`. Do not mutate GitHub state.
3. Identify the comparison base and inspect the complete diff. Do not assume `main` when the pull request specifies another target.
4. List every observable behavior change, including CLI behavior, schema or state compatibility, failure aftermath, and platform-specific behavior that is not obvious from the title.
5. Map each acceptance criterion to implementation and test evidence as **verified**, **unverified**, or **violated**.

## Use the relevant review lenses

- For every implementation change, read [layer boundaries](references/layer-boundaries.md).
- For filesystem writes, persistent state, ownership, recovery, path handling, schema versions, or dry-run behavior, read [mutation safety](references/mutation-safety.md).
- For every changed or required test, read [testing evidence](references/testing-evidence.md).

Read only the lenses relevant to the review. The documents in `docs/architecture/` and `docs/specs/` remain authoritative; these references identify the evidence a reviewer should seek.

## Validate and report

Run focused tests and the relevant locked checks when the environment permits. State precisely which commands ran, their results, and why any others could not run.

Report only evidence-backed findings that affect correctness, safety, compatibility, or acceptance criteria. Sort findings by severity and cite a concrete affected location. Do not turn a preferred implementation style into a finding when the documented contract is satisfied.

```markdown
## Findings

- P1 — Short imperative title ([path](path:line))
  Explain the reachable scenario, the violated contract, and the smallest required correction.

## Acceptance coverage

| Criterion | Status | Implementation and test evidence, or gap |
| --- | --- | --- |
| ... | verified / unverified / violated | ... |

## Validation

- Commands run and their results.
- Commands not run and the concrete reason.
```

If no finding is supported by evidence, say so explicitly.
