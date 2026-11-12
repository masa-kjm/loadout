---
name: propose-loadout-change
description: Draft evidence-based Loadout issue, commit, branch, or pull-request text. Use when preparing change proposals without mutating Git or GitHub state.
---

# Propose Loadout Changes

Propose text only; do not change Git or GitHub state. Use `CONTRIBUTING.md` for workflow and the owning architecture or specification for behavior.

## Gather evidence

1. Read `CONTRIBUTING.md`, `docs/README.md`, and the documents that own the proposed behavior.
2. For an existing change, inspect the branch and diff against its proposed base; inspect a supplied linked issue read-only before claiming it is addressed. Use `bash -lic` for `gh` as required by the repository instructions.
3. For a new issue, establish the problem, affected contract, expected outcome, safety or compatibility implications, and testable non-goals. Do not invent results or identifiers.
4. Read the reference that matches the requested output:

   - For an issue title or body, read [issue.md](references/issue.md).
   - For a commit subject or body, read [commit-message.md](references/commit-message.md).
   - For a pull-request title or body, read [pull-request.md](references/pull-request.md).
   - When the requested output is unspecified, propose an issue when a problem is not yet tracked; otherwise propose a squash-merge commit and a pull-request title and body.

## Respond

State the proposed text first, then cite the evidence that supports it. Identify unrelated changes or unavailable evidence explicitly.
