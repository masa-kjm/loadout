# CLI Specification

## Scope

This specification defines the complete v0.2.0 command surface.
Commands not listed here are not part of v0.2.0.

The CLI parses input, presents diagnostics and plans, obtains confirmation, and maps outcomes to exit status.
It does not make ownership, planner, filesystem, or state decisions outside the lifecycle.

## Declaration-Selection Options

The commands that operate on portable declarations accept:

```text
--config <path>
```

It selects the portable environment configuration as defined by [Configuration](configuration.md).
When omitted, Loadout uses `loadout.yaml` or the platform default configuration path.

For `validate`, `plan`, and `apply`, the optional positional `<profile-id>` selects exactly one discovered root profile.
It is a profile ID, not a file path.

These commands do not accept `--profile`.
More than one positional profile ID is an input error.

`diff` has no declaration-selection options because it inspects the platform state repository rather than a portable desired state.

## Commands

### `loadout validate`

```text
loadout validate [--config <path>] [--all | <profile-id>]
```

With a positional profile ID, this command validates that root profile and its include closure.
Without a profile ID or `--all`, it validates the environment configuration's `default_profile` and its include closure.
`--all` validates every discovered profile as a possible root profile and MUST NOT be combined with a positional profile ID.

When neither a profile ID nor `--all` is supplied, `default_profile` is REQUIRED.

The command performs parsing, resolution, and static validation.
It may inspect configuration files and store-source entry kinds, but it MUST NOT inspect a resource target, acquire the state lock, write state, or mutate the filesystem.

### `loadout diff`

```text
loadout diff
```

`diff` reports drift between Known state and Actual state for every resource recorded in the platform state repository.
It does not read `loadout.yaml`, an environment configuration, a profile, or a local store; it has no `--config` or positional profile option.

For each Known file-link resource, `diff` performs the no-follow target and parent observation required by [File Links](file-link.md).
It reports whether the target is the expected link, missing, another link, another entry kind, or unreachable through a safe parent path.
It also reports any active operation and each action with `pending`, `running`, or `uncertain` status.

`diff` does not produce an executable Plan, plan a repair, reconcile an operation, acquire the exclusive state lock, write state, create a directory, or mutate a target.
An absent state file represents an empty Known set and produces a successful empty report.

### `loadout plan`

```text
loadout plan [--config <path>] [<profile-id>]
```

The command resolves the selected root profile, loads Known state, inspects relevant targets, and produces a Plan.
Without a positional profile ID, the environment configuration MUST provide `default_profile`.

`plan` performs no mutation.
It does not acquire the exclusive state lock, write state, create an operation record, create a directory, or alter a target.

### `loadout apply`

```text
loadout apply [--config <path>] [<profile-id>] [--yes] [--dry-run]
```

Without a positional profile ID, the environment configuration MUST provide `default_profile`.
Apply always generates a fresh plan; it never accepts a previously displayed plan as input.

After successful preflight, a normal apply displays the executable Plan before mutation.
When standard input and standard error are terminals, it asks for confirmation and proceeds only after an affirmative response.
When the session is non-interactive, `--yes` is REQUIRED; otherwise apply fails without mutation.

`--yes` does not bypass conflicts, ownership checks, preflight, path safety, or post-condition verification.

`--dry-run` performs the dry-run lifecycle defined by [Lifecycle](lifecycle.md).
It does not ask for confirmation, and `--yes` has no effect with it.

## Output

The default output is human-readable text.
Its wording and column layout are not a machine-readable output contract in v0.2.0.

For `plan` and `apply`, output identifies:

- whether the plan is executable or blocked;
- every planned action and its fully qualified resource ID;
- each action's target and reason; and
- every blocking diagnostic with the affected resource or path when available.

For `validate`, output identifies the profile or profiles checked and every diagnostic.

For `diff`, output identifies every inspected Known resource, its target, its observation result, and every unfinished or uncertain operation.
Observed drift is reportable state, not a `diff` runtime failure.

## Exit Status

| Status | Meaning |
| --- | --- |
| `0` | The command completed successfully. A plan may contain actions or only `noop` actions, and a `diff` report may contain drift. |
| `1` | A runtime failure occurred, including an execution or state-commit failure. Earlier verified actions may remain applied. |
| `2` | Input was invalid, a plan was blocked, preflight failed, recovery is uncertain, confirmation was declined, or required non-interactive confirmation was absent. No new target mutation is performed for this outcome. |

An `apply` that has already completed one or more verified actions and then fails exits with `1`.
It never reports success merely because some earlier actions were committed.

## Excluded Commands

v0.2.0 does not provide `init`, configuration editing, profile listing or display, resource listing or display, resource import, partial apply, task execution, copy materialization, directory materialization, remote store management, forceful takeover, rollback, or parallel execution.
