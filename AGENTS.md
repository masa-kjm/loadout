# Loadout Agent Instructions

Follow these instructions for every change in this repository.

## Project Status

Loadout is being rebuilt for v0.2.0.
v0.1 is retired, unsupported, and has no compatibility contract with v0.2.0.
The `v0.1.0` and `legacy/v0.1-final` tags preserve the historical implementation; do not change them.

The existing `crates/`, legacy tests, and scripts are historical material only.
They must not determine v0.2.0 behavior, architecture, schemas, or compatibility decisions.

## Sources of Truth

Before changing v0.2.0 behavior, read the documents that own it:

- [`docs/README.md`](docs/README.md) explains document authority and v0.2.0 scope.
- [`docs/architecture/`](docs/architecture/README.md) owns responsibility and safety boundaries.
- [`docs/specs/`](docs/specs/README.md) owns observable behavior, schemas, state transitions, CLI contracts, and failure aftermath.
- [`docs/development/testing.md`](docs/development/testing.md) owns required test evidence.

Architecture and specifications are authoritative.
`docs/future/` is non-binding and may identify constraints for later work, but it does not change v0.2.0 behavior.
`tmp/draft/` and any other draft material are exploratory only and must never override published documentation.

If requirements conflict or leave a safety-, ownership-, persistence-, platform-, or schema-sensitive choice unclear, report the ambiguity before making that choice.

## v0.2.0 Architecture

Keep the lifecycle boundaries explicit:

```text
Declared Configuration
  -> Resolve and Validate
  -> Resolved Desired + Known + Actual
  -> Planner
  -> Plan
  -> Executor
  -> Verify and Commit State
```

- The planner is pure: `Resolved Desired + Known + Actual -> Plan`.
- The resolver and validator do not inspect managed targets, mutate the filesystem, or write state.
- The planner does not perform I/O, acquire locks, mutate state, or render terminal output.
- The executor performs only actions already selected by the Plan. It may reject an action after an immediate safety recheck, but it must not replan or choose a different action.
- The state repository exclusively owns locks, operation records, Known state, and atomic commits.
- Resource implementations must not write state, format CLI output, or accept unresolved YAML, store IDs, home shorthand, or relative paths.

Build the v0.2.0 implementation from a new, narrow foundation.
An isolated legacy algorithm may be mined only after it has been checked against the v0.2.0 contract; do not carry forward legacy resource models, command semantics, compatibility paths, or broad abstractions.

## Design Principles

### Keep public contracts narrow

Public contracts are expensive to change.
Treat CLI syntax and behavior, configuration and schema fields, serialized state formats, diagnostics intended for automation, and other user-observable behavior as compatibility boundaries.
Do not expose new concepts publicly unless necessary.
Prefer changing internal implementations over expanding or changing public contracts.

### Preserve architectural boundaries

Define domain models and interfaces so that each layer has a clear responsibility and cannot rely on implementation details from other layers.
Prefer explicit types and narrow interfaces over shared mutable state or cross-layer shortcuts.

- Keep parsing and loading, resolution, validation, inspection, planning, execution, and persistence separate.
- The planner operates only on resolved domain models. It must not access the filesystem or parse configuration.
- The executor executes an already-decided Plan. It must not reimplement planning decisions.
- Keep filesystem- and platform-specific behavior behind the filesystem and resource-execution boundary.
- Do not put domain decisions in the CLI or rendering layer.
- Do not leak persistence representations into domain logic unless a domain boundary requires them.

### Make invalid architecture difficult to express

Prefer types, module visibility, constructors, and interfaces that enforce invariants structurally rather than relying on comments or caller discipline.
Keep APIs minimal; do not make a type or function public unless another boundary requires it.

### Optimize for changeability

Before introducing an abstraction, identify the boundary or expected axis of change it protects.
Avoid speculative abstractions, but do not couple independent responsibilities merely because the current implementation is small.

## Safety Invariants

Preserve every applicable invariant:

- Never delete, replace, follow, or adopt an unmanaged or unexpected filesystem entry.
- Remove or replace a file link only when both Known state and current Actual observation prove the expected owned link.
- A conflict, invalid precondition, unsupported platform condition, failed preflight, validation failure, or dry run performs no target mutation.
- Recheck containment, parent safety, target kind, source safety, and action-specific ownership immediately before filesystem mutation.
- Write `running` before a mutation. Update Known state only after the exact post-condition has been verified and commit it atomically with `succeeded`.
- After an attempted mutation, classify the result from recorded preconditions and post-conditions, not from an operating-system return value alone.
- Treat an unprovable result as `uncertain`; do not retry the old action automatically.
- Apply always creates a fresh plan after recovery. It never resumes or reinterprets an old plan.
- Preserve deterministic v0.2.0 action phases and fully qualified resource-ID tie-breaking. YAML mapping order and include order are not execution-order controls.

## Engineering and Tests

- Write code, comments, user-facing documentation, commit messages, and pull-request text in English.
- Keep user-visible behavior, its owning specification, and its tests aligned in the same change.
- Add evidence at the narrowest suitable layer: pure domain, filesystem contract, state durability, executor integration, CLI acceptance, or platform conformance.
- Test every new mutation path for success, zero-mutation rejection, ownership protection, post-mutation failure, and recovery where applicable.
- Use disposable test directories only. Never use a real home directory, XDG/AppData directory, source store, or repository state directory in a mutation test.
- Once the v0.2.0 Rust package exists, run the relevant formatter, linter, and tests before handoff. Report checks that could not run and why.

## Project Skills

Source skills live in `.claude/skills/` and are synchronized for Codex discovery by `scripts/sync-codex-skills.sh`.

- `review-loadout-v0-2`: reviews v0.2 contracts, architecture boundaries, ownership and mutation safety, state consistency, compatibility, and required test evidence.
- `propose-loadout-change`: drafts evidence-based issue, commit, and pull-request text without mutating Git or GitHub state.
- `test-design-loadout-v0-2`: designs state-transition, crash-recovery, path-safety, durability, and Unix/Windows conformance evidence from v0.2 contracts.


## Repository and External State

Read [`CONTRIBUTING.md`](CONTRIBUTING.md) before work involving issues, branches, commits, pull requests, tags, releases, or other GitHub state.
Do not push, publish, create or modify GitHub releases, alter tags, delete user data, or make other external or irreversible changes unless the user explicitly requests the exact operation.
