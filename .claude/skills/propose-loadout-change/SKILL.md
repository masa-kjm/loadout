---
name: propose-loadout-change
description: Draft evidence-based Loadout v0.2 issue, commit, and pull-request text. Use when proposing an issue or its acceptance criteria, naming a commit or branch, or preparing a pull-request title and body.
---

# Propose Loadout Changes

Propose text only. Do not create issues, commits, branches, pull requests, or other GitHub or Git state.
Follow `CONTRIBUTING.md` as the source of truth.
Git is required when the requested text describes an existing change. GitHub lookup is optional and read-only.

## Gather evidence

1. Read `CONTRIBUTING.md` and the documents that own the proposed behavior. For v0.2 behavior, architecture, and test evidence, start from `docs/README.md`.
2. For a commit, branch, or pull-request proposal, read the current branch name, supplied title, and diff against the proposed base. Inspect changed paths and relevant diff; do not infer intent from a branch name alone.
3. For an issue proposal, establish the problem, affected contract, intended user-visible outcome, safety or compatibility implications, and testable non-goals. Do not invent an implementation, test result, or issue number.
4. When an actual linked issue is supplied, inspect it read-only before claiming that a change addresses it. Invoke `gh` through `bash -lic` as required by `AGENTS.md`. If it is unavailable, unauthenticated, or the issue cannot be read, state that limitation and continue from the available local evidence.
5. Read the reference that matches the requested output:

   - For an issue title or body, read [issue.md](references/issue.md).
   - For a commit subject or body, read [commit-message.md](references/commit-message.md).
   - For a pull-request title or body, read [pull-request.md](references/pull-request.md).
   - When the requested output is unspecified, propose an issue when a problem is not yet tracked; otherwise propose a squash-merge commit and a pull-request title and body.

## Respond

State the proposed text first, then list the branch, changed behavior, specifications, and inspected issue criteria that support it.
Only offer an alternative when the type, scope, or pull-request framing is genuinely ambiguous, and explain the distinction in one sentence.
If the diff contains unrelated changes, identify that as a PR-scoping concern instead of hiding it behind a broad message.
