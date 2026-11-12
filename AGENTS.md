# Loadout Agent Instructions

Follow these instructions for changes in this repository.

## Authority and scope

Read [documentation authority](docs/README.md), then the documents that own the changed behavior: [architecture](docs/architecture/README.md) for responsibilities and safety boundaries, [specifications](docs/specs/README.md) for observable contracts, and [testing strategy](docs/development/testing.md) for required evidence.
Architecture and specifications are authoritative; `docs/future/`, `local/`, and other drafts are not.
Do not derive current behavior from a historical schema or release section when a current section supersedes it.
If the authoritative documents conflict or leave a safety-, ownership-, persistence-, platform-, or schema-sensitive choice unclear, report the ambiguity before deciding it.

## Changes and validation

- Keep parsing, resolution, observation, planning, execution, and state persistence within their [architecture boundaries](docs/architecture/boundaries.md).
- Keep public CLI, schema, state, and diagnostic contracts narrow; change them only through their owning specification.
- For target mutations, preserve the ownership, immediate recheck, post-condition, operation-record, and recovery contracts in the relevant [resource](docs/specs/README.md), [lifecycle](docs/specs/lifecycle.md), and [state](docs/specs/state-and-recovery.md) specifications. Do not infer a stronger external-filesystem concurrency guarantee than [specified](docs/specs/file-link.md#external-filesystem-concurrency).
- Keep observable behavior, its owning specification, and test evidence aligned. Use the narrowest suitable test layer and cover success, zero-mutation rejection, ownership protection, post-mutation failure, and recovery for each new mutation path where applicable.
- Use disposable directories for mutation tests; never use a real home, XDG/AppData, source store, or repository state directory.
- Write code, comments, user-facing documentation, commit messages, and pull-request text in English. Keep each sentence in a comment or doc comment on one line.
- Run relevant formatter, linter, and tests before handoff; report checks that could not run and why.

For a staged pull-request review, assess a named intermediate commit against its declared scope and the final pull-request contract. It must fail closed and preserve applicable safety invariants; assess the aggregate worktree against the full contract at final review.

## Project workflow

Read [CONTRIBUTING.md](CONTRIBUTING.md) before work involving issues, branches, commits, pull requests, tags, releases, or other GitHub state. Do not mutate GitHub state, create or change Git branches or tags, push, publish, delete user data, or make other external or irreversible changes without an explicit request for that operation.

Project skill sources live in `.claude/skills/`; `scripts/sync-codex-skills.sh` generates the Codex discovery copies in `.agents/skills/`. Keep skill guidance derived from the authoritative documents.
