---
name: prepare-loadout-release
description: Prepare or validate a Loadout v0.4 release candidate and matching vX.Y.Z tag. Use for release metadata, candidate readiness, tag readiness, or release-workflow follow-up; not for ordinary feature work.
---

# Prepare Loadout Release

Prepare a focused, reviewable v0.4 release candidate. Do not infer a target version or perform a merge, tag, push, publication, workflow dispatch, or other external action unless the user explicitly requests it.

## Establish the release stage

1. Determine whether the request is candidate preparation, local candidate validation, post-merge tag readiness, or post-tag workflow follow-up.
2. Read `CONTRIBUTING.md`, the root `Cargo.toml`, the top of `CHANGELOG.md`, and `.github/workflows/release.yml` before changing release metadata.
3. Require an exact stable tag in `vX.Y.Z` form. The release branch convention is `release/vX.Y.Z`.
4. Inspect the worktree and preserve unrelated changes. Keep implementation changes out of a release-only pull request unless they are separately reviewed in scope.

## Prepare and review

Read [the release contract](references/release-contract.md) before deciding which release metadata or documentation needs updating.

Use the adjacent metadata checker for the intended tag. Read [release validation](references/release-validation.md) before running it or interpreting its result.

Use `review-loadout-v0-2` when the candidate changes v0.4 behavior, platform claims, or release automation. A metadata-only candidate is reviewed against the release contract and its complete diff.

## Complete only with explicit authority

After the release pull request is reviewed and required checks pass, merging, tagging, pushing, and dispatching a workflow each require explicit user authorization. Do not publish a crate manually: release automation distributes archives only.

After an authorized tag push, inspect the Release workflow before reporting the release as available.

## Handoff

Report the target version, changed release files, validation results, unrun checks, and release state. Distinguish a prepared candidate, a pushed tag, and a completed GitHub Release.
