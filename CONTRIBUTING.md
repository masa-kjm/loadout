# Contributing to Loadout

Keep the default branch internally consistent, reviewable, and aligned with the current contract in [`docs/`](docs/README.md).

## Sources of Truth

Before proposing or implementing a behavior change, read the documents that own it:

- [`docs/README.md`](docs/README.md) defines document authority and scope.
- [`docs/architecture/`](docs/architecture/README.md) defines system responsibilities and architectural invariants.
- [`docs/specs/`](docs/specs/README.md) defines observable behavior, schemas, ownership, state transitions, CLI behavior, and failure aftermath.
- [`docs/development/testing.md`](docs/development/testing.md) defines required test evidence.

Architecture and specifications are authoritative.
`docs/future/` is non-binding; it records constraints and promotion work for later capabilities but does not modify the current contract.
Draft material is exploratory and cannot override published documentation.

## Issues

Create an issue before implementing a user-visible behavior change or bug fix.
Write it so a reviewer can determine correctness without knowing the proposed implementation.

Use this structure:

```markdown
## Problem

Describe the observed failure or required behavior and its impact.

## Reproduction

1. List the smallest reliable steps, when applicable.

## Expected behavior

- Describe the user-visible result and failure aftermath.

## Acceptance criteria

- State independently testable requirements, including safety and zero-mutation guarantees where relevant.
```

For a filesystem mutation, state the target and source scope, ownership proof, preconditions, post-condition, dry-run behavior, failure aftermath, recovery behavior, and Unix/Windows expectations.
Link the owning specification and name intentional non-goals.

## Branches

Start a focused branch from the current default branch.
Use lowercase kebab-case names and choose a prefix that describes the work:

| Purpose | Branch pattern | Example |
| --- | --- | --- |
| User-visible feature | `feat/<description>` | `feat/file-link-create` |
| Bug fix | `fix/<description>` | `fix/parent-safety-check` |
| Documentation | `docs/<description>` | `docs/inspection` |
| Refactoring | `refactor/<description>` | `refactor/planner-inputs` |
| Tests | `test/<description>` | `test/recovery-contract` |
| Tooling or maintenance | `chore/<description>` | `chore/release-ci` |
| Release preparation | `release/vX.Y.Z` | `release/v0.1.2` |

Do not mix unrelated behavior changes in one branch.

## Commits

Use Conventional Commits with an optional scope:

```text
<type>(<scope>): <imperative summary>
```

Use `feat`, `fix`, `docs`, `refactor`, `test`, `chore`, or `ci` as the type.
Use scopes for responsibility or behavior areas, not issue numbers.

Examples:

```text
docs: promote DirectoryLink contracts
feat(file-link): create a verified managed link
test(recovery): retain an unprovable operation as uncertain
ci: add contract checks
```

Make each commit coherent and avoid drive-by formatting or unrelated refactors.
Do not describe a non-release snapshot with a release version tag.

## Pull Requests and Review

Open pull requests against `main` and link their issue when one exists.
Do not push directly to `main`.

For behavior changes, describe the affected contract, safety properties, tests run, platform limitations, and checks that could not run.
Map every acceptance criterion to implementation and test evidence, especially for ownership, filesystem mutation, persistence, recovery, schema, and compatibility-sensitive work.

Before requesting review:

- Reconcile with the current default branch.
- Update the owning documentation when an observable contract changes.
- Add the test evidence required by [`docs/development/testing.md`](docs/development/testing.md).
- Include a negative and zero-mutation case for every new mutation path.
- Run the relevant checks available for the current workspace and report checks that could not run.

`review-loadout-v0-2`, `propose-loadout-change`, `test-design-loadout-v0-2`, and `maintain-loadout-context` are available project skills.

## Validation

For documentation-only work, at minimum run `git diff --check` and verify affected relative links.

Run the relevant commands before handoff when the environment permits:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
```

Do not claim an unrun check passed.

## Releases and External State

The release contract is implemented by [`.github/workflows/release.yml`](.github/workflows/release.yml). Use `Cargo.toml` as the release version source; an exact `vX.Y.Z` tag must match that version, have a `## vX.Y.Z` heading in `CHANGELOG.md`, and point to a commit reachable from `main`.

`workflow_dispatch` builds candidate archives without creating a GitHub Release. A matching tag push validates the version and locked tests before the workflow publishes binary archives with SHA-256 sidecars. The workflow does not publish to crates.io.

For a failed or partial release, inspect the workflow and release state. Do not move or recreate the tag, overwrite assets, or rerun blindly; use a new patch version when a new immutable release is required.

Do not move, replace, or force-push the archived `v0.1.0` or `legacy/v0.1-final` tags.
