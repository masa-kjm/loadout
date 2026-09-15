# End-to-End Tests

This directory is reserved for black-box tests that execute the compiled Loadout CLI in a disposable environment.

The v0.2 test environment must provide isolated home, configuration, state, and local-store directories. It must copy version-controlled fixtures into those directories and must never use a developer's real home, XDG directories, AppData directories, or repository-owned state.

## Current Scope

The first scenario set is documented in [scenarios/README.md](scenarios/README.md). It covers the smallest useful v0.2 lifecycle: validate, plan, apply, repeated apply, diff, dry-run, and blocked plans.

The Linux Docker and Windows Sandbox runners execute the v0.2 CLI directly. They share the same fixture and scenario contract; the platform-specific runner is responsible only for creating the disposable environment, transferring the binary and fixture, and collecting logs.

## Fixture Policy

Shared deterministic inputs live in [../fixtures/](../fixtures/). Fixtures specific to a future sandbox bundle may live below `tests/e2e/fixtures/`, but generated copies, logs, temporary state, and generated sandbox definitions must remain outside version-controlled fixtures.

## Test Layers

End-to-end scenarios complement, rather than replace, the focused tests described in [Testing Strategy](../../docs/development/testing.md):

- CLI acceptance tests prove arguments, confirmation, dry-run behavior, output categories, and exit statuses.
- Executor integration tests prove state and filesystem outcomes using disposable directories.
- Platform conformance tests prove native Unix and Windows filesystem behavior.
- Manual sandbox sessions are useful for platform investigation and debugging, but are not regression evidence by themselves.
