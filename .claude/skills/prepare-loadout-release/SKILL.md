---
name: prepare-loadout-release
description: Prepare or validate a Loadout release candidate and matching vX.Y.Z tag. Use for release metadata, tag readiness, or release workflow follow-up.
---

# Prepare Loadout Release

Prepare a focused, reviewable release candidate for the requested version. Do not infer a target version or perform a merge, tag, push, publication, or workflow dispatch without an explicit request for that operation.

## Establish the release stage

1. Identify the requested release stage and exact `vX.Y.Z` version.
2. Read `CONTRIBUTING.md`, `Cargo.toml`, `CHANGELOG.md`, and `.github/workflows/release.yml`; inspect the worktree before changing metadata.
3. Keep the candidate focused on release metadata and supporting documentation.

## Prepare and review

Read [the release contract](references/release-contract.md) before deciding which release metadata or documentation needs updating.

Use the adjacent metadata checker for the intended tag. Read [release validation](references/release-validation.md) before running it or interpreting its result.

Review the complete diff against the release contract. For behavior or platform changes, also use `review-loadout-v0-2`.

## Complete only with explicit authority

Merging, tagging, pushing, and dispatching a workflow each require an explicit request. The release workflow distributes archives only.

After an authorized tag push, inspect the Release workflow before reporting the release as available.

## Handoff

Report the target version, changed release files, validation results, unrun checks, and release state. Distinguish a prepared candidate, a pushed tag, and a completed GitHub Release.
